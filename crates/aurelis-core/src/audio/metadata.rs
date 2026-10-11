use std::error::Error;
use std::fs::File;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use symphonia::core::{
    io::MediaSourceStream,
    meta::{StandardTagKey, Value},
    probe::Hint,
};
use symphonia::default::get_probe;

/// Universal metadata object used throughout AURELIS.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct AudioMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,

    pub track: Option<u32>,
    pub disc: Option<u32>,

    pub duration: Option<Duration>,

    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,

    pub codec: String,
    pub bit_depth: Option<u16>,
    /// Number of bits per encoded audio sample. The cache keeps the legacy bitrate key.
    #[serde(rename = "bitrate")]
    pub bits_per_coded_sample: Option<u32>,
}

impl AudioMetadata {
    /// Trim harmless boundary whitespace and NUL padding from imported text.
    pub(crate) fn normalize(&mut self) {
        normalize_optional_text(&mut self.title);
        normalize_optional_text(&mut self.artist);
        normalize_optional_text(&mut self.album);
        normalize_optional_text(&mut self.album_artist);
        normalize_optional_text(&mut self.genre);
        normalize_optional_text(&mut self.year);
    }

    /// Read metadata from an audio file.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn Error>> {
        let path = path.as_ref();

        let file = File::open(path)?;

        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();

        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let probed = get_probe().format(&hint, mss, &Default::default(), &Default::default())?;

        // IMPORTANT:
        // metadata() requires &mut self.
        let mut format = probed.format;

        let track = format
            .default_track()
            .ok_or("No default audio track found")?;

        let params = &track.codec_params;

        let mut meta = AudioMetadata {
            sample_rate: params.sample_rate,

            channels: params.channels.map(|c| c.count() as u16),

            codec: codec_name(params.codec),

            bit_depth: params.bits_per_sample.map(|b| b as u16),

            bits_per_coded_sample: params.bits_per_coded_sample,

            ..Default::default()
        };

        if let (Some(frames), Some(rate)) = (params.n_frames, params.sample_rate) {
            meta.duration = Some(Duration::from_secs_f64(frames as f64 / rate as f64));
        }

        if let Some(revision) = format.metadata().current() {
            for tag in revision.tags() {
                let value = value_to_string(&tag.value);

                match tag.std_key {
                    Some(StandardTagKey::TrackTitle) => {
                        meta.title = Some(value);
                    }

                    Some(StandardTagKey::Artist) => {
                        meta.artist = Some(value);
                    }

                    Some(StandardTagKey::Album) => {
                        meta.album = Some(value);
                    }

                    Some(StandardTagKey::AlbumArtist) => {
                        meta.album_artist = Some(value);
                    }

                    Some(StandardTagKey::Genre) => {
                        meta.genre = Some(value);
                    }

                    Some(StandardTagKey::Date) => {
                        meta.year = Some(value);
                    }

                    Some(StandardTagKey::TrackNumber) => {
                        meta.track = value.parse().ok();
                    }

                    Some(StandardTagKey::DiscNumber) => {
                        meta.disc = value.parse().ok();
                    }

                    _ => {}
                }
            }
        }

        meta.normalize();
        Ok(meta)
    }

    /// Format duration as mm:ss.
    pub fn duration_string(&self) -> String {
        match self.duration {
            Some(duration) => {
                let total = duration.as_secs();

                format!("{:02}:{:02}", total / 60, total % 60)
            }

            None => "--:--".to_string(),
        }
    }
}

fn normalize_optional_text(value: &mut Option<String>) {
    let Some(text) = value.take() else {
        return;
    };
    let normalized =
        text.trim_matches(|character: char| character == '\0' || character.is_whitespace());
    if !normalized.is_empty() {
        *value = Some(normalized.to_owned());
    }
}

/// Convert Symphonia's internal codec identifier
/// into a user-friendly codec name.
///
/// Symphonia 0.5.5 exposes the numeric codec value
/// through its Debug representation while the
/// CodecType tuple constructor is private.
///
/// Therefore AURELIS maps the observed codec IDs
/// without constructing CodecType values directly.
fn codec_name(codec: symphonia::core::codecs::CodecType) -> String {
    let debug = format!("{codec:?}");

    codec_name_from_debug(&debug)
}

/// Convert Symphonia's Debug representation
/// into a readable codec name.
fn codec_name_from_debug(debug: &str) -> String {
    let codec_id = debug
        .strip_prefix("CodecType(")
        .and_then(|value| value.strip_suffix(')'))
        .and_then(|value| value.parse::<u32>().ok());

    match codec_id {
        // OGG Vorbis
        Some(4096) => "Vorbis".to_string(),

        // MP3
        Some(4099) => "MP3".to_string(),

        // AAC / MPEG-4 Audio
        Some(4100) => "AAC".to_string(),

        // FLAC
        Some(8192) => "FLAC".to_string(),

        // WAV / PCM
        Some(264) => "PCM".to_string(),

        // Unknown codec.
        //
        // Keep the internal ID visible so we can
        // identify and map it later.
        Some(id) => format!("Unknown (CodecType({id}))"),

        None => "Unknown".to_string(),
    }
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),

        Value::UnsignedInt(n) => n.to_string(),

        Value::SignedInt(n) => n.to_string(),

        Value::Float(n) => n.to_string(),

        Value::Boolean(b) => b.to_string(),

        _ => format!("{value:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_string_formats_correctly() {
        let mut meta = AudioMetadata::default();

        meta.duration = Some(Duration::from_secs(200));

        assert_eq!(meta.duration_string(), "03:20");
    }

    #[test]
    fn default_metadata_is_empty() {
        let meta = AudioMetadata::default();

        assert!(meta.title.is_none());

        assert!(meta.artist.is_none());

        assert_eq!(meta.codec, "");
    }

    #[test]
    fn coded_bits_per_sample_round_trips_with_the_existing_cache_key() {
        let metadata = AudioMetadata {
            bits_per_coded_sample: Some(24),
            ..AudioMetadata::default()
        };

        let encoded = serde_json::to_string(&metadata).expect("serialize cached metadata");
        assert!(encoded.contains("\"bitrate\":24"));

        let decoded: AudioMetadata =
            serde_json::from_str(r#"{"bitrate":24}"#).expect("load existing cache metadata");
        assert_eq!(decoded.bits_per_coded_sample, Some(24));
    }

    #[test]
    fn normalize_keeps_regular_metadata_unchanged() {
        let mut metadata = AudioMetadata {
            title: Some("A Song".to_owned()),
            artist: Some("An Artist".to_owned()),
            ..AudioMetadata::default()
        };

        metadata.normalize();

        assert_eq!(metadata.title.as_deref(), Some("A Song"));
        assert_eq!(metadata.artist.as_deref(), Some("An Artist"));
    }

    #[test]
    fn normalize_trims_boundary_whitespace_and_trailing_nuls() {
        let mut metadata = AudioMetadata {
            title: Some("  Song title\0\0  ".to_owned()),
            artist: Some("\tArtist  Name\0".to_owned()),
            ..AudioMetadata::default()
        };

        metadata.normalize();

        assert_eq!(metadata.title.as_deref(), Some("Song title"));
        assert_eq!(metadata.artist.as_deref(), Some("Artist  Name"));
    }

    #[test]
    fn normalize_turns_empty_text_into_missing_metadata() {
        let mut metadata = AudioMetadata {
            album: Some(" \0\t\0 ".to_owned()),
            year: Some(String::new()),
            ..AudioMetadata::default()
        };

        metadata.normalize();

        assert_eq!(metadata.album, None);
        assert_eq!(metadata.year, None);
    }

    #[test]
    fn normalize_preserves_unicode_and_internal_spacing() {
        let mut metadata = AudioMetadata {
            title: Some("  東京  と  音楽  ".to_owned()),
            ..AudioMetadata::default()
        };

        metadata.normalize();

        assert_eq!(metadata.title.as_deref(), Some("東京  と  音楽"));
    }

    #[test]
    fn vorbis_codec_name_is_correct() {
        assert_eq!(codec_name_from_debug("CodecType(4096)"), "Vorbis");
    }

    #[test]
    fn mp3_codec_name_is_correct() {
        assert_eq!(codec_name_from_debug("CodecType(4099)"), "MP3");
    }

    #[test]
    fn aac_codec_name_is_correct() {
        assert_eq!(codec_name_from_debug("CodecType(4100)"), "AAC");
    }

    #[test]
    fn flac_codec_name_is_correct() {
        assert_eq!(codec_name_from_debug("CodecType(8192)"), "FLAC");
    }

    #[test]
    fn pcm_codec_name_is_correct() {
        assert_eq!(codec_name_from_debug("CodecType(264)"), "PCM");
    }

    #[test]
    fn unknown_codec_is_preserved() {
        assert_eq!(
            codec_name_from_debug("CodecType(9999)"),
            "Unknown (CodecType(9999))"
        );
    }

    #[test]
    fn invalid_codec_debug_is_unknown() {
        assert_eq!(codec_name_from_debug("SomethingElse"), "Unknown");
    }
}
