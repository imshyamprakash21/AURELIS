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
                 * Surround -> stereo:
                 *
                 * Standard fold-down for 3 (L R C), 4 (L R Ls Rs),
                 * 5.1 (L R C LFE Ls Rs) and 7.1 (L R C LFE Ls Rs Sl Sr),
                 * the channel order used by FLAC, WAV and Symphonia.
                 *
                 * The averaging fallback below is only correct for layouts
                 * where neighbouring channels belong to the same side; for
                 * 5.1 it would send R and C into the left output and the
                 * surrounds and LFE into the right one.
                 */
                (3 | 4 | 6 | 8, 2) => {
                    let (left, right) = fold_down_to_stereo(input_frame);

                    samples.push(left);
                    samples.push(right);
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

/// Gain applied to the centre and surround channels when folding down to
/// stereo (-3 dB, ITU-R BS.775).
const MINUS_3_DB: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Fold one 3, 4, 6 or 8 channel frame down to (left, right).
///
/// Centre and surrounds are mixed in at -3 dB and the LFE channel is dropped.
/// The result is normalised by the sum of the gains, so a full-scale signal in
/// every channel cannot exceed full scale.
fn fold_down_to_stereo(frame: &[f32]) -> (f32, f32) {
    let c = MINUS_3_DB;

    match frame.len() {
        // L R C
        3 => {
            let g = 1.0 / (1.0 + c);
            (g * (frame[0] + c * frame[2]), g * (frame[1] + c * frame[2]))
        }

        // L R Ls Rs
        4 => {
            let g = 1.0 / (1.0 + c);
            (g * (frame[0] + c * frame[2]), g * (frame[1] + c * frame[3]))
        }

        // L R C LFE Ls Rs
        6 => {
            let g = 1.0 / (1.0 + 2.0 * c);
            (
                g * (frame[0] + c * frame[2] + c * frame[4]),
                g * (frame[1] + c * frame[2] + c * frame[5]),
            )
        }

        // L R C LFE Ls Rs Sl Sr
        8 => {
            let g = 1.0 / (1.0 + 3.0 * c);
            (
                g * (frame[0] + c * frame[2] + c * frame[4] + c * frame[6]),
                g * (frame[1] + c * frame[2] + c * frame[5] + c * frame[7]),
            )
        }

        // The caller only routes 3/4/6/8 channel frames here.
        _ => unreachable!("fold_down_to_stereo called with {} channels", frame.len()),
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

    /// One 5.1 frame in the standard FLAC/WAV order: L R C LFE Ls Rs.
    fn fold_5_1(frame: [f32; 6]) -> (f32, f32) {
        let input = PcmBuffer::new(AudioSpec::new(48_000, 6), frame.to_vec());
        let output = AudioChannelConverter::convert(&input, AudioSpec::new(48_000, 2)).unwrap();

        (output.samples[0], output.samples[1])
    }

    #[test]
    fn surround_5_1_front_channels_stay_on_their_own_side() {
        let (left, right) = fold_5_1([0.0, 1.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(left, 0.0, "right-only signal leaked into the left output");
        assert!(right > 0.0);

        let (left, right) = fold_5_1([1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        assert!(left > 0.0);
        assert_eq!(right, 0.0, "left-only signal leaked into the right output");
    }

    #[test]
    fn surround_5_1_rear_channels_stay_on_their_own_side() {
        let (left, right) = fold_5_1([0.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        assert!(left > 0.0);
        assert_eq!(right, 0.0, "left-surround leaked into the right output");

        let (left, right) = fold_5_1([0.0, 0.0, 0.0, 0.0, 0.0, 1.0]);
        assert_eq!(left, 0.0, "right-surround leaked into the left output");
        assert!(right > 0.0);
    }

    #[test]
    fn surround_5_1_centre_reaches_both_sides_equally() {
        let (left, right) = fold_5_1([0.0, 0.0, 1.0, 0.0, 0.0, 0.0]);

        assert!(left > 0.0);
        assert!((left - right).abs() < 1e-6);
    }

    #[test]
    fn surround_5_1_drops_the_lfe_channel() {
        let (left, right) = fold_5_1([0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

        assert_eq!((left, right), (0.0, 0.0), "LFE must not be mixed into stereo");
    }

    #[test]
    fn surround_fold_down_cannot_clip_on_full_scale_input() {
        for channels in [3usize, 4, 6, 8] {
            let input = PcmBuffer::new(AudioSpec::new(48_000, channels), vec![1.0; channels]);
            let output = AudioChannelConverter::convert(&input, AudioSpec::new(48_000, 2)).unwrap();

            for sample in output.samples {
                assert!(sample <= 1.0 + 1e-6, "{channels}-channel fold-down clipped: {sample}");
            }
        }
    }
}
