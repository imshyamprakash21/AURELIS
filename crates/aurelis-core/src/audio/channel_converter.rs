use std::error::Error;

use super::types::{AudioSpec, PcmBuffer};

/// Convert interleaved PCM between different channel counts.
///
/// The converter operates on f32 samples and does not change
/// the sample rate.
pub struct AudioChannelConverter;

impl AudioChannelConverter {
    pub fn convert(input: &PcmBuffer, output_spec: AudioSpec) -> Result<PcmBuffer, Box<dyn Error>> {
        if input.spec.sample_rate != output_spec.sample_rate {
            return Err("Channel conversion requires matching sample rates".into());
        }

        if input.spec.channels == 0 || output_spec.channels == 0 {
            return Err("Channel conversion requires non-zero channel counts".into());
        }

        if input.spec.channels == output_spec.channels {
            return Ok(PcmBuffer::new(output_spec, input.samples.clone()));
        }

        let input_channels = input.spec.channels;

        let output_channels = output_spec.channels;

        let frames = input.frame_count();

        let mut samples = Vec::with_capacity(frames * output_channels);

        for frame in 0..frames {
            let input_start = frame * input_channels;

            let input_frame = &input.samples[input_start..input_start + input_channels];

            match (input_channels, output_channels) {
                /*
                 * Mono → stereo:
                 *
                 * M → L
                 * M → R
                 */
                (1, 2) => {
                    let mono = input_frame[0];

                    samples.push(mono);
                    samples.push(mono);
                }

                /*
                 * Stereo → mono:
                 *
                 * Average L and R.
                 */
                (2, 1) => {
                    let mono = (input_frame[0] + input_frame[1]) * 0.5;

                    samples.push(mono);
                }

                /*
                 * General conversion:
                 *
                 * - If output has fewer channels,
                 *   average source channels that map to
                 *   each output channel.
                 *
                 * - If output has more channels,
                 *   distribute the source channels across
                 *   the output channels.
                 *
                 * This gives AURELIS a safe fallback for
                 * uncommon channel layouts.
                 */
                _ => {
                    for output_channel in 0..output_channels {
                        let source_channel = output_channel * input_channels / output_channels;

                        let next_source_channel = ((output_channel + 1) * input_channels
                            / output_channels)
                            .max(source_channel + 1);

                        let end = next_source_channel.min(input_channels);

                        let mut sum = 0.0_f32;

                        let mut count = 0usize;

                        for channel in source_channel..end {
                            sum += input_frame[channel];

                            count += 1;
                        }

                        let value = if count == 0 {
                            input_frame[source_channel.min(input_channels - 1)]
                        } else {
                            sum / count as f32
                        };

                        samples.push(value);
                    }
                }
            }
        }

        Ok(PcmBuffer::new(output_spec, samples))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_to_stereo_duplicates_samples() {
        let input = PcmBuffer::new(AudioSpec::new(44_100, 1), vec![0.25, -0.5, 1.0]);

        let output = AudioChannelConverter::convert(&input, AudioSpec::new(44_100, 2)).unwrap();

        assert_eq!(output.samples, vec![0.25, 0.25, -0.5, -0.5, 1.0, 1.0,]);
    }

    #[test]
    fn stereo_to_mono_averages_channels() {
        let input = PcmBuffer::new(AudioSpec::new(44_100, 2), vec![1.0, 0.0, 0.5, -0.5]);

        let output = AudioChannelConverter::convert(&input, AudioSpec::new(44_100, 1)).unwrap();

        assert_eq!(output.samples, vec![0.5, 0.0,]);
    }

    #[test]
    fn identical_channels_are_unchanged() {
        let input = PcmBuffer::new(AudioSpec::new(48_000, 2), vec![0.1, 0.2, 0.3, 0.4]);

        let output = AudioChannelConverter::convert(&input, AudioSpec::new(48_000, 2)).unwrap();

        assert_eq!(output.samples, input.samples);
    }

    #[test]
    fn sample_rate_mismatch_is_rejected() {
        let input = PcmBuffer::new(AudioSpec::new(44_100, 1), vec![0.5]);

        let result = AudioChannelConverter::convert(&input, AudioSpec::new(48_000, 2));

        assert!(result.is_err());
    }
}
