use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread,
    time::Duration,
};

use rustfft::{Fft, FftPlanner, num_complex::Complex};

use super::ring_buffer::AudioRingBuffer;

pub const SPECTRUM_BAND_COUNT: usize = 40;

const FFT_SIZE: usize = 4096;
const HOP_SIZE: usize = 1024;
const ANALYSIS_BUFFER_CAPACITY: usize = 16_384;
const ANALYSIS_BATCH_SIZE: usize = 1024;
const MIN_FREQUENCY_HZ: f32 = 30.0;
const MAX_FREQUENCY_HZ: f32 = 16_000.0;
const FLOOR_DB: f32 = -72.0;
const RELEASE_SECONDS: f32 = 0.25;
const IDLE_UPDATE_SECONDS: f32 = 0.01;

/// A copy of the most recently analyzed 40-band output spectrum.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpectrumSnapshot {
    bands: [f32; SPECTRUM_BAND_COUNT],
}

impl SpectrumSnapshot {
    /// Normalized band energy values, ordered from low to high frequency.
    pub const fn bands(&self) -> &[f32; SPECTRUM_BAND_COUNT] {
        &self.bands
    }
}

impl Default for SpectrumSnapshot {
    fn default() -> Self {
        Self {
            bands: [0.0; SPECTRUM_BAND_COUNT],
        }
    }
}

/// Lock-free spectrum values shared with the audio-output analysis worker.
pub(crate) struct SpectrumShared {
    bands: [AtomicU32; SPECTRUM_BAND_COUNT],
}

impl SpectrumShared {
    pub(crate) fn new() -> Self {
        Self {
            bands: std::array::from_fn(|_| AtomicU32::new(0.0_f32.to_bits())),
        }
    }

    pub(crate) fn snapshot(&self) -> SpectrumSnapshot {
        SpectrumSnapshot {
            bands: std::array::from_fn(|index| {
                f32::from_bits(self.bands[index].load(Ordering::Relaxed))
            }),
        }
    }

    pub(crate) fn publish(&self, bands: &[f32; SPECTRUM_BAND_COUNT]) {
        for (shared, &value) in self.bands.iter().zip(bands) {
            shared.store(value.to_bits(), Ordering::Relaxed);
        }
    }

    pub(crate) fn clear(&self) {
        for band in &self.bands {
            band.store(0.0_f32.to_bits(), Ordering::Relaxed);
        }
    }
}

/// Owns the non-real-time worker that turns callback PCM into spectrum data.
pub(crate) struct SpectrumProcessor {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl SpectrumProcessor {
    pub(crate) fn start(
        sample_rate: u32,
        shared: Arc<SpectrumShared>,
    ) -> std::io::Result<(Self, Arc<AudioRingBuffer>)> {
        let input = Arc::new(AudioRingBuffer::new(ANALYSIS_BUFFER_CAPACITY));
        let worker_input = Arc::clone(&input);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("aurelis-spectrum".to_owned())
            .spawn(move || {
                let mut analyzer = SpectrumAnalyzer::new(sample_rate);
                let mut samples = [0.0_f32; ANALYSIS_BATCH_SIZE];

                while !worker_stop.load(Ordering::Relaxed) {
                    let read = worker_input.pop(&mut samples);
                    if read == 0 {
                        analyzer.decay(IDLE_UPDATE_SECONDS);
                        shared.publish(&analyzer.bands);
                        thread::sleep(Duration::from_millis(10));
                    } else if analyzer.push_samples(&samples[..read]) {
                        shared.publish(&analyzer.bands);
                    }
                }
            })?;

        Ok((
            Self {
                stop,
                handle: Some(handle),
            },
            input,
        ))
    }
}

impl Drop for SpectrumProcessor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct SpectrumAnalyzer {
    sample_rate: u32,
    fft: Arc<dyn Fft<f32>>,
    fft_scratch: Vec<Complex<f32>>,
    fft_buffer: Vec<Complex<f32>>,
    sample_window: Vec<f32>,
    hann_window: Vec<f32>,
    window_sum: f32,
    band_ranges: [(usize, usize); SPECTRUM_BAND_COUNT],
    #[cfg(test)]
    band_centers_hz: [f32; SPECTRUM_BAND_COUNT],
    bands: [f32; SPECTRUM_BAND_COUNT],
    write_index: usize,
    samples_filled: usize,
    samples_until_hop: usize,
}

impl SpectrumAnalyzer {
    fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1);
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let fft_scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        let fft_buffer = vec![Complex::default(); FFT_SIZE];
        let sample_window = vec![0.0; FFT_SIZE];
        let hann_window: Vec<_> = (0..FFT_SIZE)
            .map(|index| {
                0.5 - 0.5 * (std::f32::consts::TAU * index as f32 / (FFT_SIZE - 1) as f32).cos()
            })
            .collect();
        let window_sum = hann_window.iter().sum();
        let band_ranges = build_band_ranges(sample_rate);
        #[cfg(test)]
        let band_centers_hz = build_band_centers(sample_rate);

        Self {
            sample_rate,
            fft,
            fft_scratch,
            fft_buffer,
            sample_window,
            hann_window,
            window_sum,
            band_ranges,
            #[cfg(test)]
            band_centers_hz,
            bands: [0.0; SPECTRUM_BAND_COUNT],
            write_index: 0,
            samples_filled: 0,
            samples_until_hop: HOP_SIZE,
        }
    }

    fn push_samples(&mut self, samples: &[f32]) -> bool {
        let mut updated = false;
        for &sample in samples {
            self.sample_window[self.write_index] = if sample.is_finite() { sample } else { 0.0 };
            self.write_index = (self.write_index + 1) % FFT_SIZE;

            if self.samples_filled < FFT_SIZE {
                self.samples_filled += 1;
                if self.samples_filled == FFT_SIZE {
                    self.analyze_window();
                    self.samples_until_hop = HOP_SIZE;
                    updated = true;
                }
            } else {
                self.samples_until_hop -= 1;
                if self.samples_until_hop == 0 {
                    self.analyze_window();
                    self.samples_until_hop = HOP_SIZE;
                    updated = true;
                }
            }
        }
        updated
    }

    fn decay(&mut self, elapsed_seconds: f32) {
        let release = 1.0 - (-elapsed_seconds / RELEASE_SECONDS).exp();
        for band in &mut self.bands {
            *band *= 1.0 - release;
        }
    }

    fn analyze_window(&mut self) {
        for index in 0..FFT_SIZE {
            let sample_index = (self.write_index + index) % FFT_SIZE;
            self.fft_buffer[index] = Complex::new(
                self.sample_window[sample_index] * self.hann_window[index],
                0.0,
            );
        }

        self.fft
            .process_with_scratch(&mut self.fft_buffer, &mut self.fft_scratch);

        let amplitude_scale = 2.0 / self.window_sum.max(f32::MIN_POSITIVE);
        let hop_seconds = HOP_SIZE as f32 / self.sample_rate as f32;
        for (band_index, &(first_bin, last_bin)) in self.band_ranges.iter().enumerate() {
            let mut power = 0.0;
            for bin in first_bin..=last_bin {
                power += self.fft_buffer[bin].norm_sqr();
            }

            let amplitude = power.sqrt() * amplitude_scale;
            let decibels = 20.0 * amplitude.max(1.0e-6).log10();
            let target = ((decibels - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0);
            let time_constant = if target > self.bands[band_index] {
                0.05
            } else {
                RELEASE_SECONDS
            };
            let smoothing = 1.0 - (-hop_seconds / time_constant).exp();
            self.bands[band_index] += (target - self.bands[band_index]) * smoothing;
        }
    }
}

fn frequency_limits(sample_rate: u32) -> (f32, f32) {
    let nyquist = sample_rate as f32 * 0.5;
    let maximum_hz = MAX_FREQUENCY_HZ.min(nyquist * 0.95);
    let minimum_hz = MIN_FREQUENCY_HZ.min(maximum_hz * 0.5).max(1.0);
    (minimum_hz, maximum_hz)
}

fn build_band_ranges(sample_rate: u32) -> [(usize, usize); SPECTRUM_BAND_COUNT] {
    let (minimum_hz, maximum_hz) = frequency_limits(sample_rate);
    let frequency_ratio = maximum_hz / minimum_hz;
    let max_bin = FFT_SIZE / 2;

    std::array::from_fn(|index| {
        let start_ratio = index as f32 / SPECTRUM_BAND_COUNT as f32;
        let end_ratio = (index + 1) as f32 / SPECTRUM_BAND_COUNT as f32;
        let low_hz = minimum_hz * frequency_ratio.powf(start_ratio);
        let high_hz = minimum_hz * frequency_ratio.powf(end_ratio);
        let first = (low_hz * FFT_SIZE as f32 / sample_rate as f32)
            .floor()
            .max(1.0) as usize;
        let last = (high_hz * FFT_SIZE as f32 / sample_rate as f32)
            .ceil()
            .max(first as f32 + 1.0) as usize;
        (first.min(max_bin - 1), last.min(max_bin))
    })
}

#[cfg(test)]
fn build_band_centers(sample_rate: u32) -> [f32; SPECTRUM_BAND_COUNT] {
    let (minimum_hz, maximum_hz) = frequency_limits(sample_rate);
    let frequency_ratio = maximum_hz / minimum_hz;
    std::array::from_fn(|index| {
        let start_ratio = index as f32 / SPECTRUM_BAND_COUNT as f32;
        let end_ratio = (index + 1) as f32 / SPECTRUM_BAND_COUNT as f32;
        let low_hz = minimum_hz * frequency_ratio.powf(start_ratio);
        let high_hz = minimum_hz * frequency_ratio.powf(end_ratio);
        (low_hz * high_hz).sqrt()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_places_sine_energy_near_its_frequency() {
        let sample_rate = 48_000;
        let frequency_hz = 1_000.0;
        let mut analyzer = SpectrumAnalyzer::new(sample_rate);
        let samples: Vec<_> = (0..FFT_SIZE + 3 * HOP_SIZE)
            .map(|index| {
                (std::f32::consts::TAU * frequency_hz * index as f32 / sample_rate as f32).sin()
                    * 0.5
            })
            .collect();

        assert!(analyzer.push_samples(&samples));
        let strongest_band = analyzer
            .bands
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| left.total_cmp(right))
            .map(|(index, _)| index)
            .expect("40 spectrum bands");

        assert!(
            (analyzer.band_centers_hz[strongest_band] / frequency_hz)
                .log2()
                .abs()
                < 0.2
        );
        assert!(analyzer.bands[strongest_band] > 0.7);
    }

    #[test]
    fn silence_produces_zero_spectrum_energy() {
        let mut analyzer = SpectrumAnalyzer::new(48_000);
        let samples = vec![0.0; FFT_SIZE + HOP_SIZE];

        assert!(analyzer.push_samples(&samples));
        assert_eq!(analyzer.bands, [0.0; SPECTRUM_BAND_COUNT]);
    }

    #[test]
    fn published_snapshot_is_bounded_and_copyable() {
        let shared = SpectrumShared::new();
        let values = std::array::from_fn(|index| index as f32 / 39.0);
        shared.publish(&values);

        let first = shared.snapshot();
        let second = first;
        assert_eq!(first, second);
        assert!(first.bands().iter().all(|band| (0.0..=1.0).contains(band)));
    }

    #[test]
    fn background_processor_publishes_fft_data_from_its_pcm_ring() {
        let sample_rate = 48_000;
        let frequency_hz = 1_000.0;
        let shared = Arc::new(SpectrumShared::new());
        let (processor, input) =
            SpectrumProcessor::start(sample_rate, Arc::clone(&shared)).expect("start analyzer");
        let samples: Vec<_> = (0..FFT_SIZE + 4 * HOP_SIZE)
            .map(|index| {
                (std::f32::consts::TAU * frequency_hz * index as f32 / sample_rate as f32).sin()
                    * 0.5
            })
            .collect();
        assert_eq!(input.push(&samples), samples.len());

        let mut snapshot = shared.snapshot();
        for _ in 0..100 {
            if snapshot.bands().iter().any(|band| *band > 0.7) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
            snapshot = shared.snapshot();
        }

        assert!(snapshot.bands().iter().any(|band| *band > 0.7));
        drop(processor);
    }
}
