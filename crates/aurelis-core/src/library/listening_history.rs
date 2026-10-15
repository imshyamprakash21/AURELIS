use std::{
    collections::{HashMap, VecDeque},
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};

const HISTORY_VERSION: u32 = 1;
const MAX_HISTORY_ENTRIES: usize = 4_096;
const MAX_HISTORY_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 4_096;
const MAX_TEXT_BYTES: usize = 1_024;

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

/// One real playback start, identified by its local track path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ListeningPlay {
    /// File path used to identify the track across library scans.
    pub path: PathBuf,
    /// Track title captured when playback started.
    pub title: String,
    /// Effective track artist captured when playback started.
    pub artist: String,
    /// Album captured when playback started.
    pub album: String,
    /// Genre tag captured when playback started, if present.
    #[serde(default)]
    pub genre: Option<String>,
    /// Unix time in seconds when playback started.
    pub played_at_unix_seconds: u64,
}

/// Aggregated play count for a track.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackPlayCount {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub play_count: usize,
    pub last_played_at_unix_seconds: u64,
}

/// Aggregated play count for an artist.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtistPlayCount {
    pub artist: String,
    pub play_count: usize,
    pub last_played_at_unix_seconds: u64,
}

/// Aggregated play count for a known genre tag.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenrePlayCount {
    pub genre: String,
    pub play_count: usize,
    pub last_played_at_unix_seconds: u64,
}

/// Descriptive profile calculated only from retained local playback starts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ListeningDNA {
    /// Number of playback starts in the retained history window.
    pub play_starts: usize,
    /// Distinct track paths in the retained history window.
    pub unique_tracks: usize,
    /// Distinct known artist names in the retained history window.
    pub unique_artists: usize,
    /// Additional starts for tracks already started in this history window.
    pub repeat_starts: usize,
    /// Starts whose track had a non-empty genre tag.
    pub genre_tagged_starts: usize,
    /// Most-played known genres, ordered by count and then recency.
    pub genre_distribution: Vec<GenrePlayCount>,
}

/// Data-driven listening statistics derived from saved playback starts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ListeningAnalytics {
    /// Number of retained playback starts.
    pub total_play_starts: usize,
    /// Number of distinct track paths with a retained playback start.
    pub unique_tracks: usize,
    /// Number of distinct known artist names with a retained playback start.
    pub unique_artists: usize,
    /// Recent playback starts, most recently recorded first.
    pub recent_plays: Vec<ListeningPlay>,
    /// Most-played tracks, ordered by count and then recency.
    pub top_tracks: Vec<TrackPlayCount>,
    /// Most-played known artists, ordered by count and then recency.
    pub top_artists: Vec<ArtistPlayCount>,
    /// Descriptive signals calculated from the same retained play events.
    pub listening_dna: ListeningDNA,
}

/// Bounded local history of actual playback starts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ListeningHistory {
    entries: VecDeque<ListeningPlay>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredHistory {
    version: u32,
    entries: VecDeque<ListeningPlay>,
}

impl ListeningHistory {
    /// Record a successfully started track and retain the newest entries.
    pub fn record_play(&mut self, play: ListeningPlay) -> io::Result<()> {
        validate_play(&play)?;
        self.entries.push_back(play);
        if self.entries.len() > MAX_HISTORY_ENTRIES {
            self.entries.pop_front();
        }
        Ok(())
    }

    /// Number of playback starts retained in this history.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the history contains no playback starts.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Build the small summary consumed by the desktop listening page.
    pub fn analytics(&self, recent_limit: usize, ranking_limit: usize) -> ListeningAnalytics {
        let mut tracks: HashMap<PathBuf, TrackPlayCount> = HashMap::new();
        let mut artists: HashMap<String, ArtistPlayCount> = HashMap::new();
        let mut genres: HashMap<String, GenrePlayCount> = HashMap::new();

        for play in &self.entries {
            let track = tracks
                .entry(play.path.clone())
                .or_insert_with(|| TrackPlayCount {
                    path: play.path.clone(),
                    title: play.title.clone(),
                    artist: play.artist.clone(),
                    album: play.album.clone(),
                    play_count: 0,
                    last_played_at_unix_seconds: play.played_at_unix_seconds,
                });
            track.title.clone_from(&play.title);
            track.artist.clone_from(&play.artist);
            track.album.clone_from(&play.album);
            track.play_count = track.play_count.saturating_add(1);
            track.last_played_at_unix_seconds = play.played_at_unix_seconds;

            let artist_name = play.artist.trim();
            if !artist_name.is_empty() && !artist_name.eq_ignore_ascii_case("unknown artist") {
                let key = artist_name.to_lowercase();
                let artist = artists.entry(key).or_insert_with(|| ArtistPlayCount {
                    artist: artist_name.to_owned(),
                    play_count: 0,
                    last_played_at_unix_seconds: play.played_at_unix_seconds,
                });
                artist.artist = artist_name.to_owned();
                artist.play_count = artist.play_count.saturating_add(1);
                artist.last_played_at_unix_seconds = play.played_at_unix_seconds;
            }

            if let Some(genre_name) = play
                .genre
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty() && !name.eq_ignore_ascii_case("unknown genre"))
            {
                let key = genre_name.to_lowercase();
                let genre = genres.entry(key).or_insert_with(|| GenrePlayCount {
                    genre: genre_name.to_owned(),
                    play_count: 0,
                    last_played_at_unix_seconds: play.played_at_unix_seconds,
                });
                genre.genre = genre_name.to_owned();
                genre.play_count = genre.play_count.saturating_add(1);
                genre.last_played_at_unix_seconds = play.played_at_unix_seconds;
            }
        }

        let unique_tracks = tracks.len();
        let unique_artists = artists.len();
        let repeat_starts = tracks
            .values()
            .map(|track| track.play_count.saturating_sub(1))
            .sum();
        let genre_tagged_starts = genres.values().map(|genre| genre.play_count).sum();
        let mut top_tracks: Vec<_> = tracks.into_values().collect();
        top_tracks.sort_by(|left, right| {
            right
                .play_count
                .cmp(&left.play_count)
                .then_with(|| {
                    right
                        .last_played_at_unix_seconds
                        .cmp(&left.last_played_at_unix_seconds)
                })
                .then_with(|| left.title.cmp(&right.title))
                .then_with(|| left.path.cmp(&right.path))
        });
        top_tracks.truncate(ranking_limit);

        let mut top_artists: Vec<_> = artists.into_values().collect();
        top_artists.sort_by(|left, right| {
            right
                .play_count
                .cmp(&left.play_count)
                .then_with(|| {
                    right
                        .last_played_at_unix_seconds
                        .cmp(&left.last_played_at_unix_seconds)
                })
                .then_with(|| left.artist.cmp(&right.artist))
        });
        top_artists.truncate(ranking_limit);

        let mut genre_distribution: Vec<_> = genres.into_values().collect();
        genre_distribution.sort_by(|left, right| {
            right
                .play_count
                .cmp(&left.play_count)
                .then_with(|| {
                    right
                        .last_played_at_unix_seconds
                        .cmp(&left.last_played_at_unix_seconds)
                })
                .then_with(|| left.genre.cmp(&right.genre))
        });
        genre_distribution.truncate(ranking_limit);

        ListeningAnalytics {
            total_play_starts: self.entries.len(),
            unique_tracks,
            unique_artists,
            recent_plays: self
                .entries
                .iter()
                .rev()
                .take(recent_limit)
                .cloned()
                .collect(),
            top_tracks,
            top_artists,
            listening_dna: ListeningDNA {
                play_starts: self.entries.len(),
                unique_tracks,
                unique_artists,
                repeat_starts,
                genre_tagged_starts,
                genre_distribution,
            },
        }
    }

    /// Read a bounded JSON history file.
    pub fn load_from_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let size = fs::metadata(path)?.len();
        if size > MAX_HISTORY_BYTES {
            return Err(invalid_data("listening history exceeds the size limit"));
        }
        let bytes = fs::read(path)?;
        if bytes.len() as u64 > MAX_HISTORY_BYTES {
            return Err(invalid_data("listening history exceeds the size limit"));
        }

        let stored: StoredHistory = serde_json::from_slice(&bytes)
            .map_err(|error| invalid_data(&format!("invalid listening history: {error}")))?;
        if stored.version != HISTORY_VERSION || stored.entries.len() > MAX_HISTORY_ENTRIES {
            return Err(invalid_data(
                "listening history version or entry count is invalid",
            ));
        }
        for play in &stored.entries {
            validate_play(play)?;
        }

        Ok(Self {
            entries: stored.entries,
        })
    }

    /// Save the history atomically as a bounded JSON file.
    pub fn save_to_path(&self, path: impl AsRef<Path>) -> io::Result<()> {
        if self.entries.len() > MAX_HISTORY_ENTRIES {
            return Err(invalid_data("listening history contains too many entries"));
        }
        for play in &self.entries {
            validate_play(play)?;
        }

        let stored = StoredHistory {
            version: HISTORY_VERSION,
            entries: self.entries.clone(),
        };
        let bytes = serde_json::to_vec(&stored).map_err(|error| {
            invalid_data(&format!("could not encode listening history: {error}"))
        })?;
        if bytes.len() as u64 > MAX_HISTORY_BYTES {
            return Err(invalid_data("listening history exceeds the size limit"));
        }

        save_atomically(path.as_ref(), &bytes)
    }
}

fn validate_play(play: &ListeningPlay) -> io::Result<()> {
    if play.path.as_os_str().is_empty() || play.path.to_string_lossy().len() > MAX_PATH_BYTES {
        return Err(invalid_data(
            "listening history contains an invalid track path",
        ));
    }
    if [&play.title, &play.artist, &play.album]
        .iter()
        .any(|text| text.len() > MAX_TEXT_BYTES)
    {
        return Err(invalid_data(
            "listening history contains oversized track metadata",
        ));
    }
    if play
        .genre
        .as_ref()
        .is_some_and(|genre| genre.len() > MAX_TEXT_BYTES)
    {
        return Err(invalid_data(
            "listening history contains oversized genre metadata",
        ));
    }
    Ok(())
}

fn save_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_data("listening history has no parent directory"))?;
    fs::create_dir_all(parent)?;

    let (temporary, mut file) = loop {
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".aurelis-listening-history-{}-{sequence}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let write_result = file.write_all(contents).and_then(|()| file.sync_all());
    drop(file);
    let result = write_result.and_then(|()| fs::rename(&temporary, path));

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::{ListeningHistory, ListeningPlay};

    static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "aurelis-listening-history-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create temporary directory");
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

    fn play(path: &str, title: &str, artist: &str, time: u64) -> ListeningPlay {
        ListeningPlay {
            path: PathBuf::from(path),
            title: title.to_owned(),
            artist: artist.to_owned(),
            album: "Album".to_owned(),
            genre: None,
            played_at_unix_seconds: time,
        }
    }

    #[test]
    fn history_round_trips_local_paths_and_unicode_metadata() {
        let directory = TemporaryDirectory::new();
        let path = directory.path().join("listening-history.json");
        let mut history = ListeningHistory::default();
        let mut event = play(
            "Music/Artist/アルバム/曲.flac",
            "曲名",
            " артист",
            1_700_000_000,
        );
        event.genre = Some("Jazz".to_owned());
        history.record_play(event).unwrap();

        history.save_to_path(&path).unwrap();
        let loaded = ListeningHistory::load_from_path(&path).unwrap();

        assert_eq!(loaded, history);
    }

    #[test]
    fn analytics_counts_real_track_and_artist_starts_and_orders_results() {
        let mut history = ListeningHistory::default();
        history
            .record_play(play("one.flac", "One", "Artist", 10))
            .unwrap();
        history
            .record_play(play("two.flac", "Two", "artist", 20))
            .unwrap();
        history
            .record_play(play("one.flac", "One", "Artist", 30))
            .unwrap();
        history
            .record_play(play("three.flac", "Three", "Unknown artist", 40))
            .unwrap();

        let analytics = history.analytics(2, 5);

        assert_eq!(analytics.total_play_starts, 4);
        assert_eq!(analytics.unique_tracks, 3);
        assert_eq!(analytics.unique_artists, 1);
        assert_eq!(analytics.top_tracks[0].title, "One");
        assert_eq!(analytics.top_tracks[0].play_count, 2);
        assert_eq!(analytics.top_artists[0].play_count, 3);
        assert_eq!(analytics.recent_plays[0].title, "Three");
        assert_eq!(analytics.recent_plays[1].title, "One");
    }

    #[test]
    fn listening_dna_groups_tagged_genres_and_counts_replays_in_the_window() {
        let mut history = ListeningHistory::default();
        let mut first = play("one.flac", "One", "Artist", 10);
        first.genre = Some("Jazz".to_owned());
        history.record_play(first.clone()).unwrap();
        first.genre = Some("jazz".to_owned());
        first.played_at_unix_seconds = 20;
        history.record_play(first).unwrap();
        let mut second = play("two.flac", "Two", "Other artist", 30);
        second.genre = Some("Rock".to_owned());
        history.record_play(second).unwrap();
        history
            .record_play(play("three.flac", "Three", "Unknown artist", 40))
            .unwrap();

        let dna = history.analytics(8, 5).listening_dna;

        assert_eq!(dna.play_starts, 4);
        assert_eq!(dna.unique_tracks, 3);
        assert_eq!(dna.unique_artists, 2);
        assert_eq!(dna.repeat_starts, 1);
        assert_eq!(dna.genre_tagged_starts, 3);
        assert_eq!(dna.genre_distribution[0].genre, "jazz");
        assert_eq!(dna.genre_distribution[0].play_count, 2);
        assert_eq!(dna.genre_distribution[1].genre, "Rock");
    }

    #[test]
    fn missing_or_malformed_history_is_reported() {
        let directory = TemporaryDirectory::new();
        let path = directory.path().join("listening-history.json");
        assert_eq!(
            ListeningHistory::load_from_path(&path).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        fs::write(&path, b"not json").unwrap();
        assert_eq!(
            ListeningHistory::load_from_path(&path).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn history_saved_before_genre_tags_were_added_still_loads() {
        let directory = TemporaryDirectory::new();
        let path = directory.path().join("listening-history.json");
        fs::write(
            &path,
            br#"{"version":1,"entries":[{"path":"old.flac","title":"Old","artist":"Artist","album":"Album","played_at_unix_seconds":1}]}"#,
        )
        .unwrap();

        let history = ListeningHistory::load_from_path(path).unwrap();

        assert_eq!(history.len(), 1);
        assert_eq!(history.analytics(1, 1).listening_dna.genre_tagged_starts, 0);
    }

    #[test]
    fn history_discards_the_oldest_entries_after_its_retention_limit() {
        let mut history = ListeningHistory::default();
        for index in 0..4_097 {
            history
                .record_play(play(
                    &format!("{index}.flac"),
                    &format!("Track {index}"),
                    "Artist",
                    index as u64,
                ))
                .unwrap();
        }

        assert_eq!(history.len(), 4_096);
        assert_eq!(history.analytics(1, 1).recent_plays[0].title, "Track 4096");
    }
}
