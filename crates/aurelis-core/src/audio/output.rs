use std::error::Error;
use std::time::Duration;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::{
    ring_buffer::AudioRingBuffer,
    spectrum::{SpectrumProcessor, SpectrumShared},
    types::AudioSpec,
};

/// Real-time PCM audio output backed by CPAL.
///
/// The audio callback reads directly from a
/// lock-free ring buffer, so no mutex is used
/// in the real-time audio path.
pub struct AudioOutput {
    stream: cpal::Stream,
    spec: AudioSpec,
    buffer: Arc<AudioRingBuffer>,
    needs_rebuild: Arc<AtomicBool>,
    callback_state: Arc<OutputCallbackState>,
    _spectrum_processor: Option<SpectrumProcessor>,
}

#[derive(Default)]
struct OutputCallbackState {
    playing: AtomicBool,
    callbacks_active: AtomicUsize,
}

struct OutputAnalysis<'a> {
    spectrum_buffer: Option<&'a AudioRingBuffer>,
    volume: &'a AtomicU32,
    current_volume: &'a mut f32,
}

impl OutputCallbackState {
    fn wait_until_idle(&self) {
        while self.callbacks_active.load(Ordering::SeqCst) != 0 {
            std::thread::yield_now();
        }
    }
}

impl AudioOutput {
    /// Select the best f32 output configuration.
    ///
    /// Selection policy:
    ///
    /// 1. Prefer the source channel count when
    ///    the device supports it.
    ///
    /// 2. Otherwise prefer stereo.
    ///
    /// 3. Otherwise use the closest available
    ///    channel count.
    ///
    /// For the selected channel count, prefer
    /// the source sample rate.
    ///
    /// If it is unavailable, choose a sensible
    /// supported fallback rate.
    pub fn negotiate_spec(preferred: AudioSpec) -> Result<AudioSpec, Box<dyn Error>> {
        let host = cpal::default_host();

        let device = host
            .default_output_device()
            .ok_or("No default audio output device found")?;

        let ranges: Vec<_> = device
            .supported_output_configs()?
            .filter(|range| range.sample_format() == cpal::SampleFormat::F32)
            .collect();

        if ranges.is_empty() {
            return Err("No f32 output configuration is available".into());
        }

        let output_channels = if ranges
            .iter()
            .any(|range| range.channels() as usize == preferred.channels)
        {
            preferred.channels
        } else if ranges.iter().any(|range| range.channels() == 2) {
            2
        } else {
            ranges
                .iter()
                .map(|range| range.channels() as usize)
                .min_by_key(|&channels| channels.abs_diff(preferred.channels))
                .ok_or("Unable to select output channel count")?
        };

        let channel_ranges: Vec<_> = ranges
            .iter()
            .filter(|range| range.channels() as usize == output_channels)
            .collect();

        let rate_is_supported = |rate: u32| {
            channel_ranges
                .iter()
                .any(|range| range.min_sample_rate() <= rate && rate <= range.max_sample_rate())
        };

        let rate = if rate_is_supported(preferred.sample_rate) {
            preferred.sample_rate
        } else {
            [48_000, 44_100, 96_000, 88_200, 192_000, 176_400]
                .into_iter()
                .find(|&rate| rate_is_supported(rate))
                .or_else(|| {
                    channel_ranges
                        .iter()
                        .map(|range| {
                            preferred
                                .sample_rate
                                .clamp(range.min_sample_rate(), range.max_sample_rate())
                        })
                        .min_by_key(|&rate| rate.abs_diff(preferred.sample_rate))
                })
                .ok_or_else(|| {
                    format!(
                        "No f32 output configuration supports {} channels",
                        output_channels
                    )
                })?
        };

        Ok(AudioSpec::new(rate, output_channels))
    }

    /// Open the selected output configuration.
    pub fn open(requested: AudioSpec) -> Result<Self, Box<dyn Error>> {
        Self::open_with_position(requested, Arc::new(AtomicU64::new(0)))
    }

    /// Open the selected output configuration and publish consumed frames
    /// through a shared atomic counter.
    pub fn open_with_position(
        requested: AudioSpec,
        played_frames: Arc<AtomicU64>,
    ) -> Result<Self, Box<dyn Error>> {
        Self::open_internal(
            requested,
            played_frames,
            None,
            Arc::new(AtomicU32::new(1.0_f32.to_bits())),
        )
    }

    pub(crate) fn open_with_spectrum(
        requested: AudioSpec,
        played_frames: Arc<AtomicU64>,
        spectrum: Arc<SpectrumShared>,
        volume: Arc<AtomicU32>,
    ) -> Result<Self, Box<dyn Error>> {
        Self::open_internal(requested, played_frames, Some(spectrum), volume)
    }

    fn open_internal(
        requested: AudioSpec,
        played_frames: Arc<AtomicU64>,
        spectrum: Option<Arc<SpectrumShared>>,
        volume: Arc<AtomicU32>,
    ) -> Result<Self, Box<dyn Error>> {
        let host = cpal::default_host();

        let device = host
            .default_output_device()
            .ok_or("No default audio output device found")?;

        let selected = Self::negotiate_spec(requested)?;

        let supported = device
            .supported_output_configs()?
            .find(|range| {
                range.channels() as usize == selected.channels
                    && range.sample_format() == cpal::SampleFormat::F32
                    && range.min_sample_rate() <= selected.sample_rate
                    && selected.sample_rate <= range.max_sample_rate()
            })
            .ok_or("Selected output configuration is no longer supported")?
            .with_sample_rate(selected.sample_rate);

        let sample_format = supported.sample_format();

        let device_spec = AudioSpec::new(supported.sample_rate(), supported.channels() as usize);

        let config: cpal::StreamConfig = supported.into();

        let buffer = Arc::new(AudioRingBuffer::new(262_144));

        let (callback_spectrum_buffer, spectrum_processor) = if let Some(spectrum) = spectrum {
            let (processor, analysis_buffer) =
                SpectrumProcessor::start(device_spec.sample_rate, spectrum)?;
            (Some(analysis_buffer), Some(processor))
        } else {
            (None, None)
        };

        let callback_buffer = Arc::clone(&buffer);
        let callback_played_frames = Arc::clone(&played_frames);
        let callback_state = Arc::new(OutputCallbackState::default());
        let callback_output_state = Arc::clone(&callback_state);
        let output_channels = device_spec.channels;
        let callback_volume = Arc::clone(&volume);
        let mut callback_current_volume = f32::from_bits(callback_volume.load(Ordering::Relaxed));

        let rebuild_flag = Arc::new(AtomicBool::new(false));

        let callback_flag = Arc::clone(&rebuild_flag);

        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_output_stream(
                config,
                move |output: &mut [f32], _| {
                    let mut analysis = OutputAnalysis {
                        spectrum_buffer: callback_spectrum_buffer.as_deref(),
                        volume: &callback_volume,
                        current_volume: &mut callback_current_volume,
                    };
                    fill_output_with_analysis(
                        output,
                        &callback_buffer,
                        output_channels,
                        &callback_played_frames,
                        &callback_output_state,
                        &mut analysis,
                    );
                },
                move |err| {
                    let message = err.to_string();

                    if is_fatal_stream_error(err.kind()) {
                        callback_flag.store(true, Ordering::Relaxed);
                    }

                    eprintln!("Audio output error: {message}");
                },
                None,
            )?,

            _ => {
                return Err("AURELIS currently supports f32 output devices only".into());
            }
        };

        stream.play()?;
        callback_state.playing.store(true, Ordering::SeqCst);

        Ok(Self {
            stream,
            spec: device_spec,
            buffer,
            needs_rebuild: rebuild_flag,
            callback_state,
            _spectrum_processor: spectrum_processor,
        })
    }

    /// Pause the CPAL output stream.
    ///
    /// The stream remains alive and can be
    /// resumed without rebuilding it.
    pub fn pause(&self) -> Result<(), Box<dyn Error>> {
        // WASAPI queues pause commands asynchronously. Gate callback access
        // first, then wait for any callback already using the SPSC ring buffer.
        self.callback_state.playing.store(false, Ordering::SeqCst);
        let result = self.stream.pause();
        self.callback_state.wait_until_idle();
        result?;

        Ok(())
    }

    /// Resume the CPAL output stream.
    pub fn resume(&self) -> Result<(), Box<dyn Error>> {
        self.stream.play()?;
        self.callback_state.playing.store(true, Ordering::SeqCst);

        Ok(())
    }

    /// Stop output for the current playback.
    ///
    /// For now this pauses the CPAL stream.
    /// The playback worker is responsible for
    /// terminating the current pipeline.
    pub fn stop(&self) -> Result<(), Box<dyn Error>> {
        self.pause()
    }

    pub fn spec(&self) -> AudioSpec {
        self.spec
    }

    pub fn push_samples(&self, samples: &[f32]) {
        if let Err(error) = self.try_push_samples(samples) {
            eprintln!("Audio output error: {error}");
        }
    }

    /// Push decoded PCM until accepted, or return if the device stream fails.
    pub fn try_push_samples(&self, samples: &[f32]) -> Result<(), Box<dyn Error>> {
        push_samples_checked(&self.buffer, &self.needs_rebuild, samples).map_err(Into::into)
    }

    pub fn pending_samples(&self) -> usize {
        self.buffer.len()
    }

    /// Discard queued PCM while the output stream is paused.
    pub fn clear_buffer(&self) -> Result<(), Box<dyn Error>> {
        if self.callback_state.playing.load(Ordering::SeqCst) {
            return Err("Cannot clear the output ring buffer while playback is active".into());
        }

        self.callback_state.wait_until_idle();
        self.buffer.clear();

        Ok(())
    }

    pub fn available_space(&self) -> usize {
        self.buffer.available_space()
    }

    /// Whether the callback is currently allowed to consume queued PCM.
    ///
    /// CPAL does not expose a synchronous stream-state query, so this tracks
    /// the callback gate kept in sync with successful play and pause requests.
    pub fn is_playing(&self) -> bool {
        self.callback_state.playing.load(Ordering::SeqCst)
    }

    pub fn needs_rebuild(&self) -> bool {
        self.needs_rebuild.load(Ordering::Relaxed)
    }

    pub fn ring_buffer(&self) -> Arc<AudioRingBuffer> {
        Arc::clone(&self.buffer)
    }
}

/// How long the producer sleeps when the ring buffer is full.
///
/// The ring holds a couple of seconds of audio and one decoded chunk is only
/// tens of milliseconds long, so waiting a few milliseconds costs nothing.
/// Spinning instead (`yield_now` in a loop) keeps a whole CPU core busy for
/// the entire track, because the producer is always faster than real time.
const PUSH_RETRY_DELAY: Duration = Duration::from_millis(2);

/// Whether a CPAL stream error means the stream is dead and must be rebuilt.
///
/// CPAL also reports problems that leave the stream running: a single xrun
/// glitch, the default device being re-routed (the stream follows it), or a
/// refused real-time priority. Aborting the track for those would turn a
/// harmless hiccup into a stopped song. Anything else, including error kinds
/// added to CPAL later, is treated as fatal: the safe default.
fn is_fatal_stream_error(kind: cpal::ErrorKind) -> bool {
    !matches!(
        kind,
        cpal::ErrorKind::Xrun | cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied
    )
}

fn push_samples_checked(
    buffer: &AudioRingBuffer,
    needs_rebuild: &AtomicBool,
    samples: &[f32],
) -> Result<(), &'static str> {
    push_samples_with_wait(buffer, needs_rebuild, samples, &mut || {
        std::thread::sleep(PUSH_RETRY_DELAY)
    })
}

/// Push until everything is accepted. `wait` runs after every attempt that
/// found the ring buffer full.
fn push_samples_with_wait(
    buffer: &AudioRingBuffer,
    needs_rebuild: &AtomicBool,
    samples: &[f32],
    wait: &mut dyn FnMut(),
) -> Result<(), &'static str> {
    let mut written = 0;

    while written < samples.len() {
        if needs_rebuild.load(Ordering::Relaxed) {
            return Err("Audio output stream failed and must be rebuilt");
        }

        let pushed = buffer.push(&samples[written..]);
        if pushed == 0 {
            wait();
            continue;
        }

        written += pushed;
    }

    Ok(())
}

#[cfg(test)]
fn fill_output(
    output: &mut [f32],
    buffer: &Arc<AudioRingBuffer>,
    channels: usize,
    played_frames: &AtomicU64,
    callback_state: &OutputCallbackState,
) {
    let volume = AtomicU32::new(1.0_f32.to_bits());
    let mut current_volume = 1.0;
    let mut analysis = OutputAnalysis {
        spectrum_buffer: None,
        volume: &volume,
        current_volume: &mut current_volume,
    };
    fill_output_with_analysis(
        output,
        buffer,
        channels,
        played_frames,
        callback_state,
        &mut analysis,
    );
}

fn fill_output_with_analysis(
    output: &mut [f32],
    buffer: &Arc<AudioRingBuffer>,
    channels: usize,
    played_frames: &AtomicU64,
    callback_state: &OutputCallbackState,
    analysis: &mut OutputAnalysis<'_>,
) {
    let already_active = callback_state
        .callbacks_active
        .fetch_add(1, Ordering::SeqCst)
        != 0;

    if already_active || !callback_state.playing.load(Ordering::SeqCst) {
        output.fill(0.0);
        callback_state
            .callbacks_active
            .fetch_sub(1, Ordering::SeqCst);
        return;
    }

    let read = buffer.pop(output);

    if channels > 0 {
        played_frames.fetch_add((read / channels) as u64, Ordering::Relaxed);

        if read > 0 {
            let requested_volume = f32::from_bits(analysis.volume.load(Ordering::Relaxed));
            let target_volume = if requested_volume.is_finite() {
                requested_volume.clamp(0.0, 1.0)
            } else {
                1.0
            };
            let volume_step = (target_volume - *analysis.current_volume) / read as f32;
            for (index, sample) in output[..read].iter_mut().enumerate() {
                *sample *= *analysis.current_volume + volume_step * (index + 1) as f32;
            }
            *analysis.current_volume = target_volume;
        }

        if let Some(spectrum_buffer) = analysis.spectrum_buffer {
            let _ = spectrum_buffer.push_mono_downmix(&output[..read], channels);
        }
    }

    if read < output.len() {
        output[read..].fill(0.0);
    }

    callback_state
        .callbacks_active
        .fetch_sub(1, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn producer_returns_when_the_audio_stream_has_failed() {
        let buffer = AudioRingBuffer::new(4);
        let needs_rebuild = AtomicBool::new(true);
        buffer.push(&[1.0, 2.0, 3.0]);

        assert_eq!(
            push_samples_checked(&buffer, &needs_rebuild, &[4.0]),
            Err("Audio output stream failed and must be rebuilt")
        );
        assert_eq!(buffer.len(), 3);
    }

    #[test]
    fn producer_pushes_all_samples_while_the_stream_is_healthy() {
        let buffer = AudioRingBuffer::new(8);
        let needs_rebuild = AtomicBool::new(false);

        push_samples_checked(&buffer, &needs_rebuild, &[1.0, 2.0, 3.0]).unwrap();

        let mut output = [0.0; 3];
        assert_eq!(buffer.pop(&mut output), 3);
        assert_eq!(output, [1.0, 2.0, 3.0]);
    }

    fn playing_callback_state() -> OutputCallbackState {
        OutputCallbackState {
            playing: AtomicBool::new(true),
            callbacks_active: AtomicUsize::new(0),
        }
    }

    #[test]
    fn available_space_decreases_after_push() {
        let rb = AudioRingBuffer::new(16);

        assert_eq!(rb.available_space(), 15);

        rb.push(&[1.0, 2.0, 3.0]);

        assert_eq!(rb.available_space(), 12);
    }

    #[test]
    fn callback_zero_fills_when_buffer_runs_empty() {
        let rb = Arc::new(AudioRingBuffer::new(16));
        let played_frames = AtomicU64::new(0);

        rb.push(&[1.0, 2.0]);

        let mut out = [0.0; 4];

        fill_output(&mut out, &rb, 2, &played_frames, &playing_callback_state());

        assert_eq!(out, [1.0, 2.0, 0.0, 0.0]);
        assert_eq!(played_frames.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn output_position_counts_only_consumed_complete_frames() {
        let rb = Arc::new(AudioRingBuffer::new(16));
        let played_frames = AtomicU64::new(0);
        rb.push(&[1.0, 2.0, 3.0, 4.0]);

        let mut out = [0.0; 6];
        fill_output(&mut out, &rb, 2, &played_frames, &playing_callback_state());

        assert_eq!(played_frames.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn callback_captures_only_consumed_output_pcm_for_spectrum_analysis() {
        let output_buffer = Arc::new(AudioRingBuffer::new(16));
        let analysis_buffer = AudioRingBuffer::new(16);
        let played_frames = AtomicU64::new(0);
        let volume = AtomicU32::new(1.0_f32.to_bits());
        let mut current_volume = 1.0;
        output_buffer.push(&[0.25, 0.75, -0.25, -0.75]);

        let mut output = [0.0; 6];
        {
            let mut analysis = OutputAnalysis {
                spectrum_buffer: Some(&analysis_buffer),
                volume: &volume,
                current_volume: &mut current_volume,
            };
            fill_output_with_analysis(
                &mut output,
                &output_buffer,
                2,
                &played_frames,
                &playing_callback_state(),
                &mut analysis,
            );
        }

        let mut analyzed = [0.0; 2];
        assert_eq!(analysis_buffer.pop(&mut analyzed), 2);
        assert_eq!(analyzed, [0.5, -0.5]);
        assert_eq!(played_frames.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn callback_scales_output_and_spectrum_with_the_selected_volume() {
        let output_buffer = Arc::new(AudioRingBuffer::new(16));
        let analysis_buffer = AudioRingBuffer::new(16);
        let played_frames = AtomicU64::new(0);
        let volume = AtomicU32::new(0.5_f32.to_bits());
        let mut current_volume = 0.5;
        output_buffer.push(&[0.25, 0.75, -0.25, -0.75]);

        let mut output = [0.0; 4];
        {
            let mut analysis = OutputAnalysis {
                spectrum_buffer: Some(&analysis_buffer),
                volume: &volume,
                current_volume: &mut current_volume,
            };
            fill_output_with_analysis(
                &mut output,
                &output_buffer,
                2,
                &played_frames,
                &playing_callback_state(),
                &mut analysis,
            );
        }

        assert_eq!(output, [0.125, 0.375, -0.125, -0.375]);
        let mut analyzed = [0.0; 2];
        assert_eq!(analysis_buffer.pop(&mut analyzed), 2);
        assert_eq!(analyzed, [0.25, -0.25]);
    }

    #[test]
    fn callback_ramps_volume_changes_across_consumed_samples() {
        let output_buffer = Arc::new(AudioRingBuffer::new(8));
        let played_frames = AtomicU64::new(0);
        let volume = AtomicU32::new(0.0_f32.to_bits());
        let mut current_volume = 1.0;
        output_buffer.push(&[1.0; 4]);

        let mut output = [0.0; 4];
        {
            let mut analysis = OutputAnalysis {
                spectrum_buffer: None,
                volume: &volume,
                current_volume: &mut current_volume,
            };
            fill_output_with_analysis(
                &mut output,
                &output_buffer,
                1,
                &played_frames,
                &playing_callback_state(),
                &mut analysis,
            );
        }

        assert_eq!(output, [0.75, 0.5, 0.25, 0.0]);
        assert_eq!(current_volume, 0.0);
    }

    #[test]
    fn paused_callback_keeps_ring_samples_and_position_unchanged() {
        let rb = Arc::new(AudioRingBuffer::new(16));
        let played_frames = AtomicU64::new(7);
        let callback_state = OutputCallbackState::default();
        rb.push(&[1.0, 2.0, 3.0, 4.0]);

        let mut out = [9.0; 4];
        fill_output(&mut out, &rb, 2, &played_frames, &callback_state);

        assert_eq!(out, [0.0; 4]);
        assert_eq!(rb.len(), 4);
        assert_eq!(played_frames.load(Ordering::Relaxed), 7);
        assert_eq!(callback_state.callbacks_active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn resumed_callback_consumes_primed_ring_samples() {
        let rb = Arc::new(AudioRingBuffer::new(16));
        let played_frames = AtomicU64::new(0);
        let callback_state = OutputCallbackState::default();
        rb.push(&[1.0, 2.0, 3.0, 4.0]);
        callback_state.playing.store(true, Ordering::SeqCst);

        let mut out = [0.0; 4];
        fill_output(&mut out, &rb, 2, &played_frames, &callback_state);

        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
        assert!(rb.is_empty());
        assert_eq!(played_frames.load(Ordering::Relaxed), 2);
        assert_eq!(callback_state.callbacks_active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn seek_stays_silent_until_fresh_samples_are_queued_and_resumed() {
        let rb = Arc::new(AudioRingBuffer::new(16));
        let played_frames = AtomicU64::new(12);
        let callback_state = playing_callback_state();
        rb.push(&[1.0, 2.0, 3.0, 4.0]);

        callback_state.playing.store(false, Ordering::SeqCst);
        let mut out = [9.0; 4];
        fill_output(&mut out, &rb, 2, &played_frames, &callback_state);
        assert_eq!(out, [0.0; 4]);
        assert_eq!(rb.len(), 4);
        assert_eq!(played_frames.load(Ordering::Relaxed), 12);

        rb.clear();
        rb.push(&[5.0, 6.0, 7.0, 8.0]);
        fill_output(&mut out, &rb, 2, &played_frames, &callback_state);
        assert_eq!(out, [0.0; 4]);
        assert_eq!(rb.len(), 4);
        assert_eq!(played_frames.load(Ordering::Relaxed), 12);

        callback_state.playing.store(true, Ordering::SeqCst);
        fill_output(&mut out, &rb, 2, &played_frames, &callback_state);
        assert_eq!(out, [5.0, 6.0, 7.0, 8.0]);
        assert!(rb.is_empty());
        assert_eq!(played_frames.load(Ordering::Relaxed), 14);
    }

    #[test]
    fn rebuild_flag_defaults_to_false() {
        let flag = AtomicBool::new(false);

        assert!(!flag.load(Ordering::Relaxed,));
    }

    #[test]
    fn producer_waits_once_per_failed_attempt_instead_of_spinning() {
        let buffer = AudioRingBuffer::new(4);
        buffer.push(&[1.0, 2.0, 3.0]); // full: capacity - 1
        let needs_rebuild = AtomicBool::new(false);
        let mut waits = 0usize;

        // The consumer frees space only after the producer has waited 3 times.
        let result = push_samples_with_wait(&buffer, &needs_rebuild, &[4.0], &mut || {
            waits += 1;

            if waits == 3 {
                let mut sink = [0.0; 3];
                buffer.pop(&mut sink);
            }
        });

        assert_eq!(result, Ok(()));
        assert_eq!(waits, 3);
    }

    #[test]
    fn production_retry_delay_is_a_real_sleep() {
        assert!(PUSH_RETRY_DELAY >= Duration::from_millis(1));
    }

    #[test]
    fn only_survivable_stream_errors_keep_playback_going() {
        use cpal::ErrorKind;

        for kind in [
            ErrorKind::Xrun,
            ErrorKind::DeviceChanged,
            ErrorKind::RealtimeDenied,
        ] {
            assert!(!is_fatal_stream_error(kind), "{kind:?} should not abort playback");
        }

        for kind in [
            ErrorKind::DeviceNotAvailable,
            ErrorKind::StreamInvalidated,
            ErrorKind::HostUnavailable,
            ErrorKind::BackendError,
            ErrorKind::Other,
        ] {
            assert!(is_fatal_stream_error(kind), "{kind:?} must trigger a rebuild");
        }
    }
}
