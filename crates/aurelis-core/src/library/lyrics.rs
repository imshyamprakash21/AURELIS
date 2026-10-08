use std::{
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use super::Track;

/// Lyrics loaded from a local sidecar file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Lyrics {
    /// Unsynchronized plain text.
    Plain { text: String },
    /// Timestamped lines ordered by timestamp.
    Synced { lines: Vec<SyncedLyricLine> },
}

impl Lyrics {
    /// Parse LRC or enhanced LRC text. If it contains no valid line
    /// timestamps, retain it as plain text so malformed sidecars remain
    /// readable.
    pub fn parse_lrc(contents: &str) -> Self {
        let mut lines = Vec::new();

        for line in contents.lines() {
            let mut remainder = line;
            let mut timestamps = Vec::new();
            let mut malformed = false;

            while remainder.starts_with('[') {
                let Some(closing_bracket) = remainder.find(']') else {
                    malformed = true;
                    break;
                };
                let tag = &remainder[1..closing_bracket];
                let Some(timestamp) = parse_timestamp(tag) else {
                    malformed = true;
                    break;
                };
                timestamps.push(timestamp);
                remainder = &remainder[closing_bracket + 1..];
            }

            if !malformed && !timestamps.is_empty() {
                let (text, segments) = parse_timed_segments(remainder)
                    .unwrap_or_else(|| (remainder.to_owned(), Vec::new()));
                for timestamp in timestamps {
                    lines.push(SyncedLyricLine {
                        timestamp,
                        text: text.clone(),
                        segments: segments.clone(),
                    });
                }
            }
        }

        if lines.is_empty() {
            Self::Plain {
                text: contents.to_owned(),
            }
        } else {
            lines.sort_by_key(SyncedLyricLine::timestamp);
            Self::Synced { lines }
        }
    }

    /// Whether this lyrics value contains no text or timed lines.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Plain { text } => text.is_empty(),
            Self::Synced { lines } => lines.is_empty(),
        }
    }

    /// Return plain text when this is not synchronized lyrics.
    pub fn plain_text(&self) -> Option<&str> {
        match self {
            Self::Plain { text } => Some(text),
            Self::Synced { .. } => None,
        }
    }

    /// Return the ordered timestamped lines when this is synchronized lyrics.
    pub fn synced_lines(&self) -> Option<&[SyncedLyricLine]> {
        match self {
            Self::Plain { .. } => None,
            Self::Synced { lines } => Some(lines),
        }
    }
}

/// One line of synchronized lyrics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncedLyricLine {
    timestamp: Duration,
    text: String,
    segments: Vec<SyncedLyricSegment>,
}

impl SyncedLyricLine {
    /// The timestamp at which this line begins.
    pub const fn timestamp(&self) -> Duration {
        self.timestamp
    }

    /// The lyric text associated with this timestamp.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Timed text segments for enhanced LRC, or an empty slice for line-timed
    /// LRC. A segment usually represents one word, but may contain a syllable
    /// or any other text fragment timed by the source file.
    pub fn segments(&self) -> &[SyncedLyricSegment] {
        &self.segments
    }
}

/// A text fragment with its own start time in enhanced LRC.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncedLyricSegment {
    timestamp: Duration,
    text: String,
}

impl SyncedLyricSegment {
    /// The timestamp at which this text fragment begins.
    pub const fn timestamp(&self) -> Duration {
        self.timestamp
    }

    /// The text fragment, including any source whitespace around it.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Load lyrics from a same-stem `.elrc` or `.lrc` file, or a `.txt` fallback.
///
/// Missing sidecars return `Ok(None)`. Enhanced LRC has precedence over LRC,
/// which has precedence over plain text. A malformed timed sidecar is
/// preserved as plain text when it has no valid line timestamps.
pub fn load_lyrics_for_track(track: &Track) -> Result<Option<Lyrics>, LyricsError> {
    let audio_path = track.path();
    for extension in ["elrc", "lrc", "txt"] {
        let path = audio_path.with_extension(extension);
        match fs::read_to_string(&path) {
            Ok(contents) => {
                let lyrics = if extension != "txt" {
                    Lyrics::parse_lrc(&contents)
                } else {
                    Lyrics::Plain { text: contents }
                };
                return Ok(Some(lyrics));
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(LyricsError { path, source }),
        }
    }
    Ok(None)
}

fn parse_timestamp(tag: &str) -> Option<Duration> {
    let (minutes, seconds) = tag.split_once(':')?;
    if minutes.is_empty() || !minutes.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let minutes = minutes.parse::<u64>().ok()?;

    let (seconds, fraction) = match seconds.split_once('.') {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => (seconds, None),
    };
    if seconds.is_empty() || !seconds.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let seconds = seconds.parse::<u64>().ok()?;
    if seconds >= 60 {
        return None;
    }

    let nanoseconds = match fraction {
        None => 0,
        Some(fraction) if !fraction.is_empty() && fraction.bytes().all(|b| b.is_ascii_digit()) => {
            let value = fraction.parse::<u32>().ok()?;
            match fraction.len() {
                1 => value.checked_mul(100_000_000)?,
                2 => value.checked_mul(10_000_000)?,
                3 => value.checked_mul(1_000_000)?,
                _ => return None,
            }
        }
        Some(_) => return None,
    };

    let total_seconds = minutes.checked_mul(60)?.checked_add(seconds)?;
    Some(Duration::new(total_seconds, nanoseconds))
}

/// Parse inline `<timestamp>` tags while preserving every untimed character.
/// Text between tags remains attached to its preceding timestamp segment.
fn parse_timed_segments(text: &str) -> Option<(String, Vec<SyncedLyricSegment>)> {
    let mut segments = Vec::new();
    let mut plain_text = String::new();
    let mut current_segment: Option<SyncedLyricSegment> = None;
    let mut pending_text = String::new();
    let mut cursor = 0;

    while let Some(open_offset) = text[cursor..].find('<') {
        let open = cursor + open_offset;
        pending_text.push_str(&text[cursor..open]);

        let tag_start = open + 1;
        let Some(close_offset) = text[tag_start..].find('>') else {
            pending_text.push_str(&text[open..]);
            cursor = text.len();
            break;
        };
        let close = tag_start + close_offset;

        if let Some(timestamp) = parse_timestamp(&text[tag_start..close]) {
            if let Some(mut segment) = current_segment.take() {
                segment.text.push_str(&pending_text);
                pending_text.clear();
                plain_text.push_str(&segment.text);
                segments.push(segment);
            }

            current_segment = Some(SyncedLyricSegment {
                timestamp,
                text: std::mem::take(&mut pending_text),
            });
        } else {
            pending_text.push_str(&text[open..=close]);
        }

        cursor = close + 1;
    }

    pending_text.push_str(&text[cursor..]);
    let mut current_segment = current_segment?;
    current_segment.text.push_str(&pending_text);
    plain_text.push_str(&current_segment.text);
    segments.push(current_segment);

    Some((plain_text, segments))
}

/// An error encountered while reading a local lyrics sidecar.
#[derive(Debug)]
pub struct LyricsError {
    path: PathBuf,
    source: io::Error,
}

impl LyricsError {
    /// The sidecar path that could not be read.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl fmt::Display for LyricsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to read lyrics sidecar {:?}: {}",
            self.path, self.source
        )
    }
}

impl Error for LyricsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{audio::AudioMetadata, library::MusicLibrary};
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("aurelis-lyrics-{}-{sequence}", std::process::id()));
            fs::create_dir(&path).expect("create temporary lyrics directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn track(path: &Path) -> Track {
        let mut library = MusicLibrary::new();
        let id = library.add_track(path, AudioMetadata::default());
        library.get(id).unwrap().clone()
    }

    #[test]
    fn parses_valid_lrc_lines() {
        let lyrics = Lyrics::parse_lrc("[00:01.25]First line\n[02:03.5]Second line");
        let lines = lyrics.synced_lines().unwrap();

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].timestamp(), Duration::from_millis(1_250));
        assert_eq!(lines[0].text(), "First line");
        assert_eq!(lines[1].timestamp(), Duration::from_millis(123_500));
        assert_eq!(lines[1].text(), "Second line");
    }

    #[test]
    fn expands_multiple_timestamps_for_one_lyric_line() {
        let lyrics = Lyrics::parse_lrc("[00:10.00][00:12.50]Repeated line");
        let lines = lyrics.synced_lines().unwrap();

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].timestamp(), Duration::from_secs(10));
        assert_eq!(lines[1].timestamp(), Duration::from_millis(12_500));
        assert!(lines.iter().all(|line| line.text() == "Repeated line"));
    }

    #[test]
    fn parses_enhanced_lrc_segments_and_preserves_spacing() {
        let lyrics = Lyrics::parse_lrc("[00:10.00]<00:10.25>First <00:10.75>second!");
        let line = &lyrics.synced_lines().unwrap()[0];

        assert_eq!(line.text(), "First second!");
        assert_eq!(line.segments().len(), 2);
        assert_eq!(
            line.segments()[0].timestamp(),
            Duration::from_millis(10_250)
        );
        assert_eq!(line.segments()[0].text(), "First ");
        assert_eq!(
            line.segments()[1].timestamp(),
            Duration::from_millis(10_750)
        );
        assert_eq!(line.segments()[1].text(), "second!");

        let unicode = Lyrics::parse_lrc("[00:11.00]<00:11.25>गीत <00:11.75>音楽");
        let unicode_line = &unicode.synced_lines().unwrap()[0];
        assert_eq!(unicode_line.text(), "गीत 音楽");
        assert_eq!(unicode_line.segments()[0].text(), "गीत ");
        assert_eq!(unicode_line.segments()[1].text(), "音楽");
    }

    #[test]
    fn malformed_inline_timestamps_remain_visible_as_plain_line_text() {
        let lyrics = Lyrics::parse_lrc("[00:01.00]First <00:xx.00>second");
        let line = &lyrics.synced_lines().unwrap()[0];

        assert_eq!(line.text(), "First <00:xx.00>second");
        assert!(line.segments().is_empty());
    }

    #[test]
    fn sorts_unsorted_timestamps_and_keeps_equal_time_order() {
        let lyrics = Lyrics::parse_lrc("[00:02.00]Later\n[00:01.00]First\n[00:01.00]Also first");
        let lines = lyrics.synced_lines().unwrap();

        assert_eq!(
            lines.iter().map(SyncedLyricLine::text).collect::<Vec<_>>(),
            ["First", "Also first", "Later"]
        );
    }

    #[test]
    fn malformed_timestamp_lines_are_ignored_when_valid_lines_exist() {
        let lyrics = Lyrics::parse_lrc("[00:60.00]Invalid\n[00:xx.00]Malformed\n[00:01.00]Valid");
        let lines = lyrics.synced_lines().unwrap();

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "Valid");
    }

    #[test]
    fn falls_back_to_plain_text_when_no_valid_timestamps_exist() {
        let source = "[00:xx.00]Malformed\nA plain lyric line";
        let lyrics = Lyrics::parse_lrc(source);

        assert_eq!(lyrics.plain_text(), Some(source));
        assert!(lyrics.synced_lines().is_none());
    }

    #[test]
    fn retains_empty_and_plain_text_lyrics() {
        assert_eq!(Lyrics::parse_lrc("").plain_text(), Some(""));
        let plain = Lyrics::parse_lrc("First line\nSecond line");
        assert_eq!(plain.plain_text(), Some("First line\nSecond line"));
    }

    #[test]
    fn loads_same_stem_lrc_and_plain_text_fallbacks() {
        let directory = TestDirectory::new();
        let audio_path = directory.path().join("song.flac");
        let lrc_path = directory.path().join("song.lrc");
        fs::write(&lrc_path, "[00:01.00]Timed").expect("write LRC sidecar");

        let synced = load_lyrics_for_track(&track(&audio_path)).unwrap().unwrap();
        assert_eq!(synced.synced_lines().unwrap()[0].text(), "Timed");

        fs::remove_file(&lrc_path).expect("remove LRC sidecar");
        let text_path = directory.path().join("song.txt");
        fs::write(&text_path, "Plain fallback").expect("write text sidecar");
        let plain = load_lyrics_for_track(&track(&audio_path)).unwrap().unwrap();
        assert_eq!(plain.plain_text(), Some("Plain fallback"));
    }

    #[test]
    fn enhanced_lrc_has_precedence_over_lrc_and_plain_text() {
        let directory = TestDirectory::new();
        let audio_path = directory.path().join("song.flac");
        fs::write(directory.path().join("song.lrc"), "[00:01.00]Line timed")
            .expect("write LRC sidecar");
        fs::write(
            directory.path().join("song.elrc"),
            "[00:02.00]<00:02.25>Word <00:02.75>timed",
        )
        .expect("write enhanced LRC sidecar");
        fs::write(directory.path().join("song.txt"), "Plain fallback").expect("write text sidecar");

        let lyrics = load_lyrics_for_track(&track(&audio_path))
            .unwrap()
            .expect("enhanced LRC sidecar");
        let line = &lyrics.synced_lines().unwrap()[0];

        assert_eq!(line.text(), "Word timed");
        assert_eq!(line.segments().len(), 2);
    }

    #[test]
    fn returns_none_when_lyrics_are_missing() {
        let directory = TestDirectory::new();
        let audio_path = directory.path().join("missing.flac");

        assert!(
            load_lyrics_for_track(&track(&audio_path))
                .unwrap()
                .is_none()
        );
    }
}
