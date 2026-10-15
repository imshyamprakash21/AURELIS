mod artwork;
mod artwork_visual;
mod grouping;
mod listening_history;
mod lyrics;
mod persistence;
mod playlist;
mod query;
mod scanner;
mod track;

pub use artwork::{Artwork, ArtworkCache, ArtworkFormat, ArtworkSource, load_artwork_for_track};
pub use artwork_visual::{
    ArtworkVisual, ArtworkVisualError, BackgroundGradient, RgbColor, TextContrast,
};
pub use grouping::{Album, Artist};
pub use listening_history::{
    ArtistPlayCount, ListeningAnalytics, ListeningHistory, ListeningPlay, TrackPlayCount,
};
pub use lyrics::{Lyrics, LyricsError, SyncedLyricLine, SyncedLyricSegment, load_lyrics_for_track};
pub use playlist::{Playlist, PlaylistCollection, PlaylistError, PlaylistId, PlaylistTrack};
pub use query::{SortKey, SortOrder, TrackQuery};
pub use scanner::{DiscoveryIssue, DiscoveryResult, is_supported_audio_file, scan_directory};
pub use track::{Track, TrackId};

use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
};

use crate::audio::AudioMetadata;

/// An in-memory collection of tracks.
///
/// Track IDs are assigned by this library and are unique for its lifetime.
/// Adding a track with a path already present updates its metadata and keeps
/// its existing ID. Paths are compared as stored; this type does not perform
/// filesystem access or canonicalize them.
#[derive(Debug)]
pub struct MusicLibrary {
    tracks: Vec<Track>,
    next_id: u64,
}

impl MusicLibrary {
    /// Create an empty library. IDs start at 1.
    pub const fn new() -> Self {
        Self {
            tracks: Vec::new(),
            next_id: 1,
        }
    }

    /// Number of tracks in the library.
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    /// Whether the library contains no tracks.
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// Add a track, or refresh the metadata of the track with the same path.
    ///
    /// The path is stored exactly as provided. This operation performs no
    /// filesystem I/O. If the path already exists in the library, its ID is
    /// retained and only its metadata is replaced.
    pub fn add_track(&mut self, path: impl Into<PathBuf>, mut metadata: AudioMetadata) -> TrackId {
        let path = path.into();
        metadata.normalize();

        if let Some(track) = self.tracks.iter_mut().find(|track| track.path() == path) {
            track.replace_metadata(metadata);
            return track.id();
        }

        let next_id = self
            .next_id
            .checked_add(1)
            .expect("MusicLibrary exhausted its TrackId space");
        let id = TrackId::new(self.next_id);
        self.next_id = next_id;
        self.tracks.push(Track::new(id, path, metadata));

        id
    }

    /// Find a track by its library-assigned ID.
    pub fn get(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|track| track.id() == id)
    }

    /// Find a track by its stored path, using path equality without
    /// canonicalizing or accessing the filesystem.
    pub fn get_by_path(&self, path: impl AsRef<Path>) -> Option<&Track> {
        let path = path.as_ref();
        self.tracks.iter().find(|track| track.path() == path)
    }

    /// Iterate over tracks in insertion order.
    pub fn iter(&self) -> std::slice::Iter<'_, Track> {
        self.tracks.iter()
    }

    /// Return read-only references to tracks matching `query`.
    ///
    /// Filtering and searching use a linear scan; matching tracks are then
    /// sorted according to the query. The library itself is not modified.
    pub fn query(&self, query: &TrackQuery) -> Vec<&Track> {
        query::execute(&self.tracks, query)
    }

    /// Group tracks into albums ordered by album title and effective album artist.
    ///
    /// Tracks in an album are ordered by disc number, track number, and path.
    /// Album-artist metadata is preferred, so compilation tracks with distinct
    /// track artists remain in the same album group.
    pub fn albums(&self) -> Vec<Album> {
        grouping::albums(&self.tracks)
    }

    /// Group tracks by their effective track artist, ordered by artist name.
    pub fn artists(&self) -> Vec<Artist> {
        grouping::artists(&self.tracks)
    }

    /// Return tracks matching an album title and effective album artist.
    /// Matching is case-insensitive and uses the same fallbacks as [`Self::albums`].
    pub fn tracks_by_album(&self, title: &str, album_artist: &str) -> Vec<&Track> {
        grouping::tracks_by_album(&self.tracks, title, album_artist)
    }

    /// Return tracks matching an effective track artist, ordered by album and
    /// then disc number, track number, and path. Matching is case-insensitive.
    pub fn tracks_by_artist(&self, artist: &str) -> Vec<&Track> {
        grouping::tracks_by_artist(&self.tracks, artist)
    }

    /// Rescan a directory while reusing metadata from the local library index
    /// for files whose size and modification time have not changed.
    ///
    /// The index is stored under `.aurelis_cache` inside `root`. If it is
    /// missing or invalid, the directory is scanned normally and the index is
    /// rebuilt. Index read or write failures do not prevent library use.
    pub fn scan_directory_cached(&mut self, root: impl AsRef<Path>) -> ScanReport {
        persistence::scan_directory_cached(self, root.as_ref())
    }

    /// Discover and read metadata for supported audio files under `root`.
    ///
    /// Discovery and metadata errors are included in the returned report.
    /// A failure for one file does not prevent other discovered files from
    /// being processed. Existing tracks are refreshed in place and retain
    /// their IDs.
    pub fn scan_directory(&mut self, root: impl AsRef<Path>) -> ScanReport {
        let DiscoveryResult { paths, issues } = scanner::scan_directory(root);
        let mut report = ScanReport {
            discovered: paths.len(),
            issues: issues.into_iter().map(ScanIssue::Discovery).collect(),
            ..ScanReport::default()
        };

        for path in paths {
            let was_present = self.get_by_path(&path).is_some();

            match AudioMetadata::from_file(&path) {
                Ok(metadata) => {
                    self.add_track(path, metadata);
                    if was_present {
                        report.updated += 1;
                    } else {
                        report.added += 1;
                    }
                }
                Err(source) => {
                    report.failed += 1;
                    report
                        .issues
                        .push(ScanIssue::Metadata(MetadataIssue { path, source }));
                }
            }
        }

        report
    }
}

/// Summary of a metadata-aware library scan.
#[derive(Debug, Default)]
pub struct ScanReport {
    /// Number of supported-extension candidate paths found by discovery.
    pub discovered: usize,
    /// Successfully parsed files newly added to the library.
    pub added: usize,
    /// Successfully parsed existing tracks whose metadata was refreshed.
    pub updated: usize,
    /// Discovered files for which metadata extraction failed.
    pub failed: usize,
    /// Discovery and metadata issues encountered during the scan.
    pub issues: Vec<ScanIssue>,
}

/// An issue encountered during discovery or metadata extraction.
#[derive(Debug)]
pub enum ScanIssue {
    /// A filesystem problem while traversing the selected directory.
    Discovery(DiscoveryIssue),
    /// A failure to read metadata from a discovered candidate file.
    Metadata(MetadataIssue),
}

/// A metadata extraction failure for a discovered candidate path.
#[derive(Debug)]
pub struct MetadataIssue {
    path: PathBuf,
    source: Box<dyn Error>,
}

impl MetadataIssue {
    /// The discovered path whose metadata could not be read.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The underlying error returned by `AudioMetadata::from_file`.
    pub fn source_error(&self) -> &(dyn Error + 'static) {
        self.source.as_ref()
    }
}

impl fmt::Display for MetadataIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to read metadata for {:?}: {}",
            self.path, self.source
        )
    }
}

impl Error for MetadataIssue {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

impl Default for MusicLibrary {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("aurelis-library-{}-{sequence}", std::process::id()));
            fs::create_dir(&path).expect("create temporary test directory");
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

    fn write_wav(path: &Path, sample_rate: u32) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create test directory tree");
        }

        // Minimal PCM16 mono WAV containing eight zero-valued samples.
        let samples = [0_u8; 16];
        let data_len = samples.len() as u32;
        let mut bytes = Vec::with_capacity(44 + samples.len());
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.extend_from_slice(&samples);

        fs::write(path, bytes).expect("write test WAV file");
    }

    fn metadata(title: Option<&str>) -> AudioMetadata {
        AudioMetadata {
            title: title.map(str::to_owned),
            ..AudioMetadata::default()
        }
    }

    #[test]
    fn new_library_starts_empty() {
        let library = MusicLibrary::new();

        assert!(library.is_empty());
        assert_eq!(library.len(), 0);
        assert_eq!(library.iter().count(), 0);
    }

    #[test]
    fn default_library_starts_empty() {
        let library = MusicLibrary::default();

        assert!(library.is_empty());
        assert_eq!(library.len(), 0);
    }

    #[test]
    fn adding_tracks_assigns_unique_ids() {
        let mut library = MusicLibrary::new();

        let first = library.add_track("Music/first.flac", metadata(Some("First")));
        let second = library.add_track("Music/second.flac", metadata(Some("Second")));

        assert_ne!(first, second);
        assert_eq!(first, TrackId::new(1));
        assert_eq!(second, TrackId::new(2));
        assert_eq!(library.len(), 2);
    }

    #[test]
    fn lookup_by_id_returns_the_track() {
        let mut library = MusicLibrary::new();
        let id = library.add_track("Music/song.flac", metadata(Some("Song")));

        let track = library.get(id).unwrap();

        assert_eq!(track.id(), id);
        assert_eq!(track.path(), PathBuf::from("Music/song.flac"));
        assert_eq!(track.metadata().title.as_deref(), Some("Song"));
    }

    #[test]
    fn lookup_by_path_returns_the_track() {
        let mut library = MusicLibrary::new();
        let id = library.add_track("Music/song.flac", metadata(Some("Song")));

        assert_eq!(library.get_by_path("Music/song.flac").unwrap().id(), id);
        assert!(library.get_by_path("Music/missing.flac").is_none());
    }

    #[test]
    fn iteration_preserves_insertion_order() {
        let mut library = MusicLibrary::new();
        library.add_track("Music/first.flac", metadata(Some("First")));
        library.add_track("Music/second.flac", metadata(Some("Second")));

        let titles: Vec<_> = library
            .iter()
            .map(|track| track.metadata().title.as_deref().unwrap())
            .collect();

        assert_eq!(titles, ["First", "Second"]);
    }

    #[test]
    fn adding_an_existing_path_updates_metadata_and_keeps_its_id() {
        let mut library = MusicLibrary::new();
        let original_id = library.add_track("Music/song.flac", metadata(Some("Old title")));
        let returned_id = library.add_track("Music/song.flac", metadata(Some("New title")));

        assert_eq!(returned_id, original_id);
        assert_eq!(library.len(), 1);
        assert_eq!(
            library
                .get(original_id)
                .unwrap()
                .metadata()
                .title
                .as_deref(),
            Some("New title")
        );
    }

    #[test]
    fn scanning_one_valid_wav_adds_track_with_metadata() {
        let directory = TestDirectory::new();
        let path = directory.path().join("song.wav");
        write_wav(&path, 44_100);
        let mut library = MusicLibrary::new();

        let report = library.scan_directory(directory.path());

        assert_eq!(report.discovered, 1);
        assert_eq!(report.added, 1);
        assert_eq!(report.updated, 0);
        assert_eq!(report.failed, 0);
        assert!(report.issues.is_empty());
        let track = library.get_by_path(&path).unwrap();
        assert_eq!(track.metadata().sample_rate, Some(44_100));
    }

    #[test]
    fn scanning_multiple_valid_files_assigns_unique_ids() {
        let directory = TestDirectory::new();
        let first_path = directory.path().join("first.wav");
        let second_path = directory.path().join("nested/second.wav");
        write_wav(&first_path, 44_100);
        write_wav(&second_path, 48_000);
        let mut library = MusicLibrary::new();

        let report = library.scan_directory(directory.path());

        assert_eq!(report.discovered, 2);
        assert_eq!(report.added, 2);
        assert_eq!(report.failed, 0);
        assert_eq!(library.len(), 2);
        assert_ne!(
            library.get_by_path(first_path).unwrap().id(),
            library.get_by_path(second_path).unwrap().id()
        );
    }

    #[test]
    fn corrupt_supported_file_is_reported_without_aborting_scan() {
        let directory = TestDirectory::new();
        let corrupt_path = directory.path().join("corrupt.mp3");
        fs::write(&corrupt_path, b"not an audio file").expect("write corrupt candidate");
        let mut library = MusicLibrary::new();

        let report = library.scan_directory(directory.path());

        assert_eq!(report.discovered, 1);
        assert_eq!(report.added, 0);
        assert_eq!(report.failed, 1);
        assert!(matches!(
            report.issues.as_slice(),
            [ScanIssue::Metadata(issue)] if issue.path() == corrupt_path
        ));
        assert!(library.is_empty());
    }

    #[test]
    fn mixed_valid_and_invalid_files_are_processed_independently() {
        let directory = TestDirectory::new();
        let valid_path = directory.path().join("valid.wav");
        write_wav(&valid_path, 44_100);
        fs::write(directory.path().join("corrupt.mp3"), b"broken")
            .expect("write corrupt candidate");
        fs::write(directory.path().join("cover.jpg"), b"image").expect("write unsupported file");
        let mut library = MusicLibrary::new();

        let report = library.scan_directory(directory.path());

        assert_eq!(report.discovered, 2);
        assert_eq!(report.added, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(library.len(), 1);
        assert!(library.get_by_path(valid_path).is_some());
        assert!(matches!(report.issues[0], ScanIssue::Metadata(_)));
    }

    #[test]
    fn rescanning_refreshes_metadata_and_preserves_track_id() {
        let directory = TestDirectory::new();
        let path = directory.path().join("song.wav");
        write_wav(&path, 44_100);
        let mut library = MusicLibrary::new();

        let first_report = library.scan_directory(directory.path());
        let original_id = library.get_by_path(&path).unwrap().id();
        assert_eq!(first_report.added, 1);

        write_wav(&path, 48_000);
        let second_report = library.scan_directory(directory.path());

        let track = library.get_by_path(&path).unwrap();
        assert_eq!(second_report.discovered, 1);
        assert_eq!(second_report.added, 0);
        assert_eq!(second_report.updated, 1);
        assert_eq!(second_report.failed, 0);
        assert_eq!(track.id(), original_id);
        assert_eq!(track.metadata().sample_rate, Some(48_000));
        assert_eq!(library.len(), 1);
    }

    #[test]
    fn scanning_empty_directory_succeeds_with_zero_counts() {
        let directory = TestDirectory::new();
        let mut library = MusicLibrary::new();

        let report = library.scan_directory(directory.path());

        assert_eq!(report.discovered, 0);
        assert_eq!(report.added, 0);
        assert_eq!(report.updated, 0);
        assert_eq!(report.failed, 0);
        assert!(report.issues.is_empty());
    }

    #[test]
    fn unsupported_files_are_not_discovered() {
        let directory = TestDirectory::new();
        fs::write(directory.path().join("cover.jpg"), b"image").expect("write unsupported file");
        fs::write(directory.path().join("notes.txt"), b"text").expect("write unsupported file");
        let mut library = MusicLibrary::new();

        let report = library.scan_directory(directory.path());

        assert_eq!(report.discovered, 0);
        assert_eq!(report.added, 0);
        assert_eq!(report.failed, 0);
        assert!(report.issues.is_empty());
        assert!(library.is_empty());
    }

    #[test]
    fn discovery_issues_are_preserved_in_scan_report() {
        let directory = TestDirectory::new();
        let missing = directory.path().join("missing");
        let mut library = MusicLibrary::new();

        let report = library.scan_directory(&missing);

        assert_eq!(report.discovered, 0);
        assert_eq!(report.failed, 0);
        assert!(matches!(
            report.issues.as_slice(),
            [ScanIssue::Discovery(issue)] if issue.path == missing
        ));
    }

    #[test]
    fn repeated_scan_does_not_create_duplicate_tracks() {
        let directory = TestDirectory::new();
        write_wav(&directory.path().join("song.wav"), 44_100);
        let mut library = MusicLibrary::new();

        let first_report = library.scan_directory(directory.path());
        let second_report = library.scan_directory(directory.path());

        assert_eq!(first_report.added, 1);
        assert_eq!(second_report.added, 0);
        assert_eq!(second_report.updated, 1);
        assert_eq!(library.len(), 1);
    }
}
