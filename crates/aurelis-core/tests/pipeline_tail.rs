//! Headless end-to-end check of the decode -> resample path.
//!
//! The loop below mirrors `decode_next_output_chunk` and the end-of-stream
//! flush in `playback.rs`, so a regression there shows up here.
//!
//! No audio device needed: this drives AudioDecoder + AudioResampler with the
//! same process()/process_final()/flush() sequence that PlaybackEngine uses,
//! on synthetic WAV files whose length is chosen to stress the final chunk.
//!
//! It verifies that no real audio is lost at the end of a track, for every
//! source rate AURELIS cares about.

use std::f64::consts::PI;
use std::io::Write;
use std::path::{Path, PathBuf};

use aurelis_core::audio::AudioSpec;
use aurelis_core::audio::decoder_stream::AudioDecoder;
use aurelis_core::audio::resampler::AudioResampler;

const OUT_RATE: u32 = 48_000;
const AMPLITUDE: f64 = 0.8;
const TOLERANCE_FRAMES: i64 = 16;

/// Write a stereo 16-bit PCM WAV containing a sine wave.
fn write_wav(path: &Path, rate: u32, frames: usize, freq: f64) {
    let channels = 2u16;
    let data_len = (frames * channels as usize * 2) as u32;
    let mut f = std::fs::File::create(path).unwrap();

    f.write_all(b"RIFF").unwrap();
    f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
    f.write_all(b"WAVEfmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap();
    f.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
    f.write_all(&channels.to_le_bytes()).unwrap();
    f.write_all(&rate.to_le_bytes()).unwrap();
    f.write_all(&(rate * channels as u32 * 2).to_le_bytes()).unwrap();
    f.write_all(&(channels * 2).to_le_bytes()).unwrap();
    f.write_all(&16u16.to_le_bytes()).unwrap();
    f.write_all(b"data").unwrap();
    f.write_all(&data_len.to_le_bytes()).unwrap();

    for n in 0..frames {
        let v = AMPLITUDE * (2.0 * PI * freq * n as f64 / rate as f64).sin();
        let s = (v * 32767.0) as i16;
        for _ in 0..channels {
            f.write_all(&s.to_le_bytes()).unwrap();
        }
    }
}

/// Same loop as playback_worker. Returns the left channel.
fn run_pipeline(path: &Path, do_flush: bool) -> Vec<f32> {
    let mut decoder = AudioDecoder::open(path).unwrap();
    let source = decoder.spec();
    let target = AudioSpec::new(OUT_RATE, source.channels);
    let mut resampler = AudioResampler::new(source, target).unwrap();

    let mut left = Vec::new();
    let mut take_left = |samples: &[f32]| {
        left.extend(samples.chunks_exact(2).map(|frame| frame[0]));
    };

    while let Some(chunk) = decoder.next_chunk().unwrap() {
        let out = if chunk.frame_count() == resampler.input_frames_next() {
            resampler.process(&chunk).unwrap()
        } else {
            resampler.process_final(&chunk).unwrap()
        };
        take_left(&out.samples);
    }

    if do_flush {
        take_left(&resampler.flush().unwrap().samples);
    }

    left
}

fn onset(buf: &[f32]) -> usize {
    buf.iter().position(|x| x.abs() > 0.1).expect("no audio found")
}

fn last_audible(buf: &[f32]) -> usize {
    buf.iter().rposition(|x| x.abs() > 0.1).expect("no audio found")
}

fn temp_wav(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("aurelis_{name}.wav"))
}

/// Number of real-audio frames, measured as (last audible - first audible).
fn audible_span(buf: &[f32]) -> i64 {
    last_audible(buf) as i64 - onset(buf) as i64
}

#[test]
fn final_chunk_and_flush_preserve_the_whole_tail() {
    // (source rate, input frames, description)
    let cases: [(u32, usize, &str); 9] = [
        (96_000, 48 * 4096 + 722, "96k, short final chunk"),
        (96_000, 48 * 4096 + 4090, "96k, nearly-full final chunk"),
        (96_000, 48 * 4096, "96k, exact multiple of 4096"),
        (96_000, 47 * 4096 + 1, "96k, 1-frame final chunk"),
        (44_100, 48 * 4096 + 722, "44.1k, short final chunk"),
        (44_100, 48 * 4096 + 4090, "44.1k, nearly-full final chunk"),
        (44_100, 48 * 4096, "44.1k, exact multiple of 4096"),
        (192_000, 48 * 4096 + 4000, "192k, final chunk"),
        (88_200, 48 * 4096 + 3000, "88.2k, final chunk"),
    ];

    for (rate, frames, label) in cases {
        let path = temp_wav(&format!("tail_{rate}_{frames}"));
        write_wav(&path, rate, frames, 1000.0);

        let ideal = (frames as f64 * OUT_RATE as f64 / rate as f64).round() as i64;
        let out = run_pipeline(&path, true);
        let _ = std::fs::remove_file(&path);

        let span = audible_span(&out);
        assert!(
            (span - ideal).abs() <= TOLERANCE_FRAMES,
            "{label}: audible span {span} vs ideal {ideal} (off by {})",
            span - ideal
        );

        // Resampler start-up delay should stay tiny (well under 5 ms).
        assert!(onset(&out) < 240, "{label}: audio starts late at {}", onset(&out));
    }
}

#[test]
fn flush_is_required_when_the_last_chunk_is_full() {
    // Documents *why* flush() exists: when the final decoder chunk is full-size
    // (or nearly), the resampler's delay line still holds real audio, and
    // skipping flush() drops it.
    let frames = 48 * 4096; // exact multiple
    let path = temp_wav("flush_required");
    write_wav(&path, 44_100, frames, 1000.0);

    let with_flush = run_pipeline(&path, true);
    let without_flush = run_pipeline(&path, false);
    let _ = std::fs::remove_file(&path);

    assert!(
        audible_span(&with_flush) > audible_span(&without_flush),
        "flush() should recover audio that is otherwise lost at the end"
    );
}
