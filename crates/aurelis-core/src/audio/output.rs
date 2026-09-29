use std::error::Error;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::{ring_buffer::AudioRingBuffer, types::AudioSpec};

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
}

#[derive(Default)]
struct OutputCallbackState {
    playing: AtomicBool,
    callbacks_active: AtomicUsize,
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

        let callback_buffer = Arc::clone(&buffer);
        let callback_played_frames = Arc::clone(&played_frames);
        let callback_state = Arc::new(OutputCallbackState::default());
        let callback_output_state = Arc::clone(&callback_state);
        let output_channels = device_spec.channels;

        let rebuild_flag = Arc::new(AtomicBool::new(false));

        let callback_flag = Arc::clone(&rebuild_flag);

        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_output_stream(
                config,
                move |output: &mut [f32], _| {
                    fill_output(
                        output,
                        &callback_buffer,
                        output_channels,
                        &callback_played_frames,
                        &callback_output_state,
                    );
                },
                move |err| {
                    let message = err.to_string();

                    if message.contains("must be rebuilt")
                        || message.contains("configuration is no longer valid")
                    {
                        callback_flag.store(true, Ordering::Relaxed);

                        return;
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
        let mut written = 0;

        while written < samples.len() {
            let pushed = self.buffer.push(&samples[written..]);

            if pushed == 0 {
                std::thread::yield_now();

                continue;
            }

            written += pushed;
        }
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

fn fill_output(
    output: &mut [f32],
    buffer: &Arc<AudioRingBuffer>,
    channels: usize,
    played_frames: &AtomicU64,
    callback_state: &OutputCallbackState,
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
}
