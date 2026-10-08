use std::collections::VecDeque;
use std::fs::File;
use std::path::{Path, PathBuf};

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::{Time, TimeBase};
use symphonia::default::{get_codecs, get_probe};

use super::stream::DEFAULT_CHUNK_FRAMES;
use super::types::{AudioSpec, PcmBuffer};

/// A streaming audio decoder.
///
/// Audio packets are decoded progressively and converted into small
/// PCM chunks. Any samples that do not fit into the current chunk are
/// retained and returned by the next call.
///
/// Some containers/codecs do not expose complete channel information
/// in their initial codec parameters. When that happens, the decoder
/// derives the actual audio specification from the first decoded packet
/// instead of making assumptions about the channel layout.
pub struct AudioDecoder {
    path: PathBuf,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    spec: AudioSpec,
    time_base: Option<TimeBase>,
    start_frame: u64,
    total_frames: Option<u64>,
    decoded_frame_cursor: u64,
    discard_until_frame: Option<u64>,
    chunk_frames: usize,
    pending_samples: VecDeque<f32>,
    finished: bool,
}

impl AudioDecoder {
    /// Open an audio file using the default chunk size.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
        Self::open_with_chunk_size(path, DEFAULT_CHUNK_FRAMES)
    }

    /// Open an audio file with a custom PCM chunk size.
    pub fn open_with_chunk_size<P: AsRef<Path>>(
        path: P,
        chunk_frames: usize,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if chunk_frames == 0 {
            return Err("Chunk size must be greater than zero".into());
        }

        let path = path.as_ref().to_path_buf();

        let file = File::open(&path)?;

        let media_source = MediaSourceStream::new(Box::new(file), Default::default());

        let hint = Hint::new();

        let probed = get_probe().format(
            &hint,
            media_source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )?;

        let mut format = probed.format;

        let track = format.default_track().ok_or("No audio track found")?;

        let track_id = track.id;
        let codec_params = track.codec_params.clone();
        let time_base = codec_params.time_base;
        let start_ts = codec_params.start_ts;
        let total_frames = codec_params.n_frames;

        let mut decoder = get_codecs().make(&codec_params, &DecoderOptions::default())?;

        /*
         * Some formats provide all information immediately.
         *
         * Other formats, such as some M4A/AAC files, may not expose
         * channel information in the initial codec parameters.
         *
         * Never assume stereo.
         *
         * If the channel count is unavailable, decode the first packet
         * and derive the actual specification from the decoded audio.
         */
        let sample_rate = codec_params.sample_rate;

        let channels = codec_params.channels.map(|channels| channels.count());

        let (sample_rate, channels, initial_samples) = match (sample_rate, channels) {
            (Some(rate), Some(channel_count)) if rate > 0 && channel_count > 0 => {
                (rate, channel_count, Vec::new())
            }

            _ => {
                let (rate, channel_count, samples) =
                    Self::decode_first_packet(&mut format, &mut decoder, track_id)?;

                (rate, channel_count, samples)
            }
        };

        if sample_rate == 0 {
            return Err("Decoded audio reported an invalid sample rate".into());
        }

        if channels == 0 {
            return Err("Decoded audio reported zero channels".into());
        }

        let spec = AudioSpec::new(sample_rate, channels);
        let effective_time_base = time_base.unwrap_or_else(|| TimeBase::new(1, sample_rate));
        let start_frame = timestamp_to_frames(start_ts, effective_time_base, sample_rate);
        let decoded_frame_cursor =
            start_frame.saturating_add((initial_samples.len() / channels) as u64);

        Ok(Self {
            path,
            format,
            decoder,
            track_id,
            spec,
            time_base,
            start_frame,
            total_frames,
            decoded_frame_cursor,
            discard_until_frame: None,
            chunk_frames,
            pending_samples: VecDeque::from(initial_samples),
            finished: false,
        })
    }

    /// Decode the first audio packet to discover the actual audio
    /// specification when container-level codec parameters are incomplete.
    fn decode_first_packet(
        format: &mut Box<dyn FormatReader>,
        decoder: &mut Box<dyn Decoder>,
        track_id: u32,
    ) -> Result<(u32, usize, Vec<f32>), Box<dyn std::error::Error>> {
        loop {
            let packet = match format.next_packet() {
                Ok(packet) => packet,

                Err(Error::ResetRequired) => {
                    return Err("Decoder reset required".into());
                }

                Err(Error::IoError(_)) => {
                    return Err("Unexpected end of file while reading audio".into());
                }

                Err(error) => {
                    return Err(Box::new(error));
                }
            };

            if packet.track_id() != track_id {
                continue;
            }

            let decoded = decoder.decode(&packet)?;

            let decoded_spec = *decoded.spec();

            let sample_rate = decoded_spec.rate;

            let channels = decoded_spec.channels.count();

            if sample_rate == 0 {
                return Err("Decoded audio reported an invalid sample rate".into());
            }

            if channels == 0 {
                return Err("Decoded audio reported zero channels".into());
            }

            let duration = decoded.capacity() as u64;

            let mut buffer = SampleBuffer::<f32>::new(duration, decoded_spec);

            buffer.copy_interleaved_ref(decoded);

            let samples = buffer.samples().iter().copied().collect::<Vec<f32>>();

            return Ok((sample_rate, channels, samples));
        }
    }

    /// Return the source file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Return the audio specification.
    pub const fn spec(&self) -> AudioSpec {
        self.spec
    }

    /// Return the configured chunk size in frames.
    pub const fn chunk_frames(&self) -> usize {
        self.chunk_frames
    }

    /// Return whether the decoder has reached the end of the file.
    pub const fn is_finished(&self) -> bool {
        self.finished
    }

    /// Return the number of samples currently waiting to be emitted.
    pub fn pending_samples(&self) -> usize {
        self.pending_samples.len()
    }

    /// Seek to a source-relative position and discard decoded preroll up to
    /// the requested frame. Returns the effective clamped position.
    pub fn seek_to(
        &mut self,
        position: std::time::Duration,
    ) -> Result<std::time::Duration, Box<dyn std::error::Error>> {
        let target_frame = duration_to_frames(position, self.spec.sample_rate);

        if let Some(total_frames) = self.total_frames {
            if target_frame >= total_frames {
                self.pending_samples.clear();
                self.finished = true;
                self.decoded_frame_cursor = self.start_frame.saturating_add(total_frames);
                self.discard_until_frame = None;

                return Ok(frames_to_duration(total_frames, self.spec.sample_rate));
            }
        }

        let time = Time::new(
            position.as_secs(),
            f64::from(position.subsec_nanos()) / 1_000_000_000.0,
        );

        let seeked = self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            },
        )?;

        self.decoder.reset();
        self.pending_samples.clear();
        self.finished = false;

        let time_base = self
            .time_base
            .unwrap_or_else(|| TimeBase::new(1, self.spec.sample_rate));
        self.decoded_frame_cursor =
            timestamp_to_frames(seeked.actual_ts, time_base, self.spec.sample_rate);
        self.discard_until_frame = Some(self.start_frame.saturating_add(target_frame));

        Ok(position)
    }

    /// Decode and return the next PCM chunk.
    ///
    /// Samples that cross a chunk boundary are retained internally
    /// instead of being discarded.
    pub fn next_chunk(&mut self) -> Result<Option<PcmBuffer>, Box<dyn std::error::Error>> {
        if self.finished && self.pending_samples.is_empty() {
            return Ok(None);
        }

        let target_samples = self.chunk_frames * self.spec.channels;

        while self.pending_samples.len() < target_samples && !self.finished {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,

                Err(Error::ResetRequired) => {
                    return Err("Decoder reset required".into());
                }

                Err(Error::IoError(_)) => {
                    self.finished = true;
                    break;
                }

                Err(error) => {
                    return Err(Box::new(error));
                }
            };

            if packet.track_id() != self.track_id {
                continue;
            }

            let decoded = self.decoder.decode(&packet)?;

            let decoded_spec = *decoded.spec();

            let actual_spec = AudioSpec::new(decoded_spec.rate, decoded_spec.channels.count());

            if actual_spec != self.spec {
                return Err("Audio format changed during decoding".into());
            }

            let duration = decoded.capacity() as u64;

            let mut buffer = SampleBuffer::<f32>::new(duration, decoded_spec);

            buffer.copy_interleaved_ref(decoded);

            self.append_decoded_samples(buffer.samples());
        }

        if self.pending_samples.is_empty() {
            return Ok(None);
        }

        let samples_to_take = self.pending_samples.len().min(target_samples);

        let samples = self
            .pending_samples
            .drain(..samples_to_take)
            .collect::<Vec<f32>>();

        Ok(Some(PcmBuffer::new(self.spec, samples)))
    }

    fn append_decoded_samples(&mut self, samples: &[f32]) {
        let channels = self.spec.channels;
        let decoded_frames = samples.len() / channels;
        let decoded_end_frame = self
            .decoded_frame_cursor
            .saturating_add(decoded_frames as u64);

        let skip_frames = self
            .discard_until_frame
            .map(|target| {
                target
                    .saturating_sub(self.decoded_frame_cursor)
                    .min(decoded_frames as u64)
            })
            .unwrap_or(0) as usize;

        let skip_samples = skip_frames * channels;
        self.pending_samples
            .extend(samples[skip_samples..].iter().copied());

        self.decoded_frame_cursor = decoded_end_frame;

        if self
            .discard_until_frame
            .is_some_and(|target| decoded_end_frame >= target)
        {
            self.discard_until_frame = None;
        }
    }
}

fn duration_to_frames(position: std::time::Duration, sample_rate: u32) -> u64 {
    (position.as_nanos() * u128::from(sample_rate) / 1_000_000_000).min(u128::from(u64::MAX)) as u64
}

fn frames_to_duration(frames: u64, sample_rate: u32) -> std::time::Duration {
    if sample_rate == 0 {
        return std::time::Duration::ZERO;
    }

    let rate = u64::from(sample_rate);
    let seconds = frames / rate;
    let nanoseconds = ((frames % rate) * 1_000_000_000 / rate) as u32;
    std::time::Duration::new(seconds, nanoseconds)
}

fn timestamp_to_frames(timestamp: u64, time_base: TimeBase, sample_rate: u32) -> u64 {
    let time = time_base.calc_time(timestamp);
    let position = std::time::Duration::from_secs(time.seconds)
        .saturating_add(std::time::Duration::from_secs_f64(time.frac));

    duration_to_frames(position, sample_rate)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("aurelis-decoder-{}-{sequence}", std::process::id()));
            fs::create_dir_all(&path).expect("create decoder test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_pcm_wav(path: &Path, sample_rate: u32, samples: &[i16]) {
        let data_len = (samples.len() * 2) as u32;
        let mut bytes = Vec::with_capacity(44 + data_len as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        fs::write(path, bytes).expect("write decoder test WAV file");
    }

    #[test]
    fn rejects_zero_chunk_size() {
        let result = AudioDecoder::open_with_chunk_size("nonexistent.flac", 0);

        assert!(result.is_err());
    }

    #[test]
    fn decodes_across_chunk_boundaries_and_finishes_cleanly() {
        let directory = TemporaryDirectory::new();
        let path = directory.path().join("chunked.wav");
        let source_samples = [
            1000, -2000, 3000, -4000, 5000, -6000, 7000, -8000, 9000, -10000,
        ];
        write_pcm_wav(&path, 8_000, &source_samples);

        let mut decoder = AudioDecoder::open_with_chunk_size(&path, 4).expect("open WAV decoder");
        assert_eq!(decoder.spec(), AudioSpec::new(8_000, 1));

        let mut chunk_sizes = Vec::new();
        let mut decoded_samples = Vec::new();
        while let Some(chunk) = decoder.next_chunk().expect("decode next WAV chunk") {
            chunk_sizes.push(chunk.frame_count());
            decoded_samples.extend(chunk.samples);
        }

        assert_eq!(chunk_sizes, [4, 4, 2]);
        assert_eq!(decoded_samples.len(), source_samples.len());
        assert!(decoded_samples.iter().any(|sample| *sample != 0.0));
        assert!(decoder.is_finished());
        assert!(
            decoder
                .next_chunk()
                .expect("read after end of stream")
                .is_none()
        );
    }

    #[test]
    fn rejects_a_malformed_audio_file() {
        let directory = TemporaryDirectory::new();
        let path = directory.path().join("broken.wav");
        fs::write(&path, b"not a valid audio file").expect("write malformed audio file");

        assert!(AudioDecoder::open(&path).is_err());
    }
}
