use std::error::Error;

use rubato::{
    Fft, FixedSync, Indexing, Resampler,
    audioadapter_buffers::direct::{SequentialSliceOfSlices, SequentialSliceOfVecs},
};

use super::types::{AudioSpec, PcmBuffer};

/// High-quality sample-rate converter for AURELIS.
///
/// The decoder preserves the source sample rate. This component converts
/// decoded PCM to the sample rate required by the output device.
///
/// Common source rates supported:
/// - 44,100 Hz
/// - 48,000 Hz
/// - 88,200 Hz
/// - 96,000 Hz
/// - 192,000 Hz
///
/// The underlying resampler is configured with `FixedSync::Input`, so
/// normal processing requires exactly `input_frames_next()` frames.
/// The final chunk of a stream is usually shorter and must therefore
/// be passed through `process_final()`.
pub struct AudioResampler {
    input_spec: AudioSpec,
    output_spec: AudioSpec,
    resampler: Fft<f32>,
}

impl AudioResampler {
    /// Create a new sample-rate converter.
    ///
    /// Channel conversion is intentionally not performed here.
    /// Input and output must have the same number of channels.
    pub fn new(input_spec: AudioSpec, output_spec: AudioSpec) -> Result<Self, Box<dyn Error>> {
        if input_spec.sample_rate == 0 || output_spec.sample_rate == 0 {
            return Err("Sample rate must be greater than zero".into());
        }

        if input_spec.channels == 0 || output_spec.channels == 0 {
            return Err("Channel count must be greater than zero".into());
        }

        if input_spec.channels != output_spec.channels {
            return Err(format!(
                "Channel conversion is not supported: {} -> {}",
                input_spec.channels, output_spec.channels
            )
            .into());
        }

        if input_spec.sample_rate == output_spec.sample_rate {
            return Err("Resampler is unnecessary when sample rates are identical".into());
        }

        let resampler = Fft::<f32>::new(
            input_spec.sample_rate as usize,
            output_spec.sample_rate as usize,
            4096,
            input_spec.channels,
            FixedSync::Input,
        )?;

        Ok(Self {
            input_spec,
            output_spec,
            resampler,
        })
    }

    /// Return the input specification.
    pub const fn input_spec(&self) -> AudioSpec {
        self.input_spec
    }

    /// Return the output specification.
    pub const fn output_spec(&self) -> AudioSpec {
        self.output_spec
    }

    /// Return the exact number of input frames required by a normal
    /// `process()` call.
    pub fn input_frames_next(&self) -> usize {
        self.resampler.input_frames_next()
    }

    /// Resample one full-size PCM buffer.
    ///
    /// The input must contain exactly `input_frames_next()` frames.
    pub fn process(&mut self, input: &PcmBuffer) -> Result<PcmBuffer, Box<dyn Error>> {
        if input.spec != self.input_spec {
            return Err("Input PCM specification does not match resampler".into());
        }

        if input.samples.is_empty() {
            return Ok(PcmBuffer::new(self.output_spec, Vec::new()));
        }

        let channels = self.input_spec.channels;

        self.validate_alignment(input, channels)?;

        let input_frames = input.frame_count();
        let expected_frames = self.input_frames_next();

        if input_frames != expected_frames {
            return Err(format!(
                "Resampler requires exactly {expected_frames} input frames, \
                 received {input_frames}. Use process_final() for the final \
                 partial chunk."
            )
            .into());
        }

        let input_channels = Self::deinterleave(input, channels);

        self.run(&input_channels, input_frames, None)
    }

    /// Resample the final, possibly short, chunk of a stream.
    ///
    /// Rubato is configured for fixed 4096-frame input blocks. Therefore
    /// the adapter receives a zero-padded 4096-frame block while
    /// `partial_len` tells Rubato how many frames contain real audio.
    pub fn process_final(&mut self, input: &PcmBuffer) -> Result<PcmBuffer, Box<dyn Error>> {
        if input.spec != self.input_spec {
            return Err("Input PCM specification does not match resampler".into());
        }

        let channels = self.input_spec.channels;
        let expected_frames = self.input_frames_next();

        if input.samples.is_empty() {
            return self.run(&vec![Vec::new(); channels], 0, Some(0));
        }

        self.validate_alignment(input, channels)?;

        let input_frames = input.frame_count();

        if input_frames > expected_frames {
            return Err(format!(
                "process_final received {input_frames} frames, but the \
                 final chunk must not exceed {expected_frames} frames"
            )
            .into());
        }

        let input_channels = Self::deinterleave(input, channels);

        self.run(&input_channels, input_frames, Some(input_frames))
    }

    /// Drain the resampler's internal filter state.
    ///
    /// This should be called once after `process_final()`.
    pub fn flush(&mut self) -> Result<PcmBuffer, Box<dyn Error>> {
        let channels = self.output_spec.channels;

        self.run(&vec![Vec::new(); channels], 0, Some(0))
    }

    /// Reset the internal resampler state.
    pub fn reset(&mut self) {
        self.resampler.reset();
    }

    /// Shared implementation for normal, final, and flush processing.
    fn run(
        &mut self,
        input_channels: &[Vec<f32>],
        input_frames: usize,
        partial_len: Option<usize>,
    ) -> Result<PcmBuffer, Box<dyn Error>> {
        let channels = self.input_spec.channels;
        let expected_frames = self.input_frames_next();

        /*
         * Rubato's FixedSync::Input resampler expects the adapter to expose
         * the complete configured input block.
         *
         * For a normal block:
         *
         *   input_frames = 4096
         *   partial_len  = None
         *
         * For the final 47-frame block:
         *
         *   input_frames = 47
         *   partial_len  = Some(47)
         *
         * The adapter still exposes 4096 frames, with the remaining
         * frames zero-padded. Rubato then processes only the first 47
         * frames as real input.
         */

        let mut padded_channels = vec![vec![0.0f32; expected_frames]; channels];

        for channel in 0..channels {
            let copy_frames = input_frames.min(input_channels[channel].len());

            padded_channels[channel][..copy_frames]
                .copy_from_slice(&input_channels[channel][..copy_frames]);
        }

        let input_adapter =
            SequentialSliceOfVecs::new(&padded_channels, channels, expected_frames)?;

        let output_frames = self.resampler.output_frames_next();

        let mut output_channels = vec![vec![0.0f32; output_frames]; channels];

        let mut output_slices: Vec<&mut [f32]> =
            output_channels.iter_mut().map(Vec::as_mut_slice).collect();

        let mut output_adapter = SequentialSliceOfSlices::new_mut(
            output_slices.as_mut_slice(),
            channels,
            output_frames,
        )?;

        let indexing = partial_len.map(|len| Indexing::new().partial_len(len));

        let (_, output_frames_produced) = self.resampler.process_into_buffer(
            &input_adapter,
            &mut output_adapter,
            indexing.as_ref(),
        )?;

        let mut interleaved = Vec::with_capacity(output_frames_produced * channels);

        for frame in 0..output_frames_produced {
            for channel in output_channels.iter() {
                interleaved.push(channel[frame]);
            }
        }

        Ok(PcmBuffer::new(self.output_spec, interleaved))
    }

    fn validate_alignment(&self, input: &PcmBuffer, channels: usize) -> Result<(), Box<dyn Error>> {
        if input.samples.len() % channels != 0 {
            return Err("PCM sample count is not aligned to channel count".into());
        }

        Ok(())
    }

    fn deinterleave(input: &PcmBuffer, channels: usize) -> Vec<Vec<f32>> {
        let input_frames = input.frame_count();

        let mut input_channels = vec![Vec::<f32>::with_capacity(input_frames); channels];

        for frame in input.samples.chunks_exact(channels) {
            for (channel, &sample) in frame.iter().enumerate() {
                input_channels[channel].push(sample);
            }
        }

        input_channels
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::types::AudioSpec;

    fn test_buffer(spec: AudioSpec, frames: usize) -> PcmBuffer {
        let samples = vec![0.0f32; frames * spec.channels];

        PcmBuffer::new(spec, samples)
    }

    #[test]
    fn resampler_accepts_44100_to_48000() {
        let input = AudioSpec::new(44_100, 2);
        let output = AudioSpec::new(48_000, 2);

        let mut resampler = AudioResampler::new(input, output).unwrap();

        let buffer = test_buffer(input, resampler.input_frames_next());

        let result = resampler.process(&buffer).unwrap();

        assert_eq!(result.spec, output);
        assert!(!result.samples.is_empty());
    }

    #[test]
    fn resampler_accepts_88200_to_48000() {
        let input = AudioSpec::new(88_200, 2);
        let output = AudioSpec::new(48_000, 2);

        let mut resampler = AudioResampler::new(input, output).unwrap();

        let buffer = test_buffer(input, resampler.input_frames_next());

        let result = resampler.process(&buffer).unwrap();

        assert_eq!(result.spec, output);
        assert!(!result.samples.is_empty());
    }

    #[test]
    fn resampler_accepts_96000_to_48000() {
        let input = AudioSpec::new(96_000, 2);
        let output = AudioSpec::new(48_000, 2);

        let mut resampler = AudioResampler::new(input, output).unwrap();

        let buffer = test_buffer(input, resampler.input_frames_next());

        let result = resampler.process(&buffer).unwrap();

        assert_eq!(result.spec, output);
        assert!(!result.samples.is_empty());
    }

    #[test]
    fn resampler_accepts_192000_to_48000() {
        let input = AudioSpec::new(192_000, 2);
        let output = AudioSpec::new(48_000, 2);

        let mut resampler = AudioResampler::new(input, output).unwrap();

        let buffer = test_buffer(input, resampler.input_frames_next());

        let result = resampler.process(&buffer).unwrap();

        assert_eq!(result.spec, output);
        assert!(!result.samples.is_empty());
    }

    #[test]
    fn resampler_rejects_channel_conversion() {
        let input = AudioSpec::new(96_000, 2);
        let output = AudioSpec::new(48_000, 1);

        assert!(AudioResampler::new(input, output).is_err());
    }

    #[test]
    fn resampler_rejects_identical_sample_rate() {
        let input = AudioSpec::new(48_000, 2);
        let output = AudioSpec::new(48_000, 2);

        assert!(AudioResampler::new(input, output).is_err());
    }

    #[test]
    fn resampler_rejects_mismatched_input_spec() {
        let input = AudioSpec::new(96_000, 2);
        let output = AudioSpec::new(48_000, 2);
        let wrong = AudioSpec::new(44_100, 2);

        let mut resampler = AudioResampler::new(input, output).unwrap();

        let buffer = test_buffer(wrong, resampler.input_frames_next());

        assert!(resampler.process(&buffer).is_err());
    }

    #[test]
    fn process_final_handles_short_trailing_chunk() {
        let input = AudioSpec::new(96_000, 2);
        let output = AudioSpec::new(48_000, 2);

        let mut resampler = AudioResampler::new(input, output).unwrap();

        let full = test_buffer(input, resampler.input_frames_next());

        for _ in 0..3 {
            resampler.process(&full).unwrap();
        }

        let tail = test_buffer(input, 47);

        let result = resampler.process_final(&tail);

        assert!(
            result.is_ok(),
            "process_final should accept a short final chunk, got {:?}",
            result.err()
        );

        let result = result.unwrap();

        assert_eq!(result.spec, output);
    }

    #[test]
    fn process_final_rejects_oversized_chunk() {
        let input = AudioSpec::new(96_000, 2);
        let output = AudioSpec::new(48_000, 2);

        let mut resampler = AudioResampler::new(input, output).unwrap();

        let too_big = test_buffer(input, resampler.input_frames_next() + 1);

        assert!(resampler.process_final(&too_big).is_err());
    }

    #[test]
    fn flush_drains_without_error_after_final_chunk() {
        let input = AudioSpec::new(96_000, 2);
        let output = AudioSpec::new(48_000, 2);

        let mut resampler = AudioResampler::new(input, output).unwrap();

        let tail = test_buffer(input, 47);

        resampler.process_final(&tail).unwrap();

        let flushed = resampler.flush();

        assert!(
            flushed.is_ok(),
            "flush should never error, got {:?}",
            flushed.err()
        );

        assert_eq!(flushed.unwrap().spec, output);
    }

    #[test]
    fn flush_is_safe_with_no_prior_processing() {
        let input = AudioSpec::new(96_000, 2);
        let output = AudioSpec::new(48_000, 2);

        let mut resampler = AudioResampler::new(input, output).unwrap();

        assert!(resampler.flush().is_ok());
    }
}
