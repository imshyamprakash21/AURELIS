use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::Duration,
};

use aurelis_core::audio::SpectrumSnapshot;
use aurelis_core::library::{
    Album, Artist, Artwork, ArtworkCache, ArtworkFormat, ArtworkSource, Lyrics, MusicLibrary,
    Track, TrackId, TrackQuery, load_lyrics_for_track,
};

use crate::player_controller::{PlaybackControlTarget, PlaybackStatus, PlayerController};

const MAX_LYRICS_CACHE_ENTRIES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesktopPlaybackState {
    Stopped,
    Playing,
    Paused,
    Finished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtworkImageFormat {
    Jpeg,
    Png,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtworkView {
    pub data: Vec<u8>,
    pub format: ArtworkImageFormat,
    pub source: String,
}

impl From<&Artwork> for ArtworkView {
    fn from(artwork: &Artwork) -> Self {
        let format = match artwork.format() {
            ArtworkFormat::Jpeg => ArtworkImageFormat::Jpeg,
            ArtworkFormat::Png => ArtworkImageFormat::Png,
        };
        let source = match artwork.source() {
            ArtworkSource::Embedded => "Embedded artwork".to_owned(),
            ArtworkSource::FileSystem { path } => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Folder artwork".to_owned()),
        };
        Self {
            data: artwork.data().to_vec(),
            format,
            source,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TrackSummary {
    pub id: TrackId,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: Option<String>,
    pub genre: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub path: PathBuf,
    pub duration: Option<Duration>,
    pub codec: String,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u16>,
    pub channels: Option<u16>,
    pub bits_per_coded_sample: Option<u32>,
}

impl From<&Track> for TrackSummary {
    fn from(track: &Track) -> Self {
        let metadata = track.metadata();
        Self {
            id: track.id(),
            title: track.display_title().into_owned(),
            artist: track.display_artist().into_owned(),
            album: track.display_album().into_owned(),
            year: metadata.year.clone(),
            genre: metadata.genre.clone(),
            track_number: metadata.track,
            disc_number: metadata.disc,
            path: track.path().to_path_buf(),
            duration: metadata.duration,
            codec: metadata.codec.clone(),
            sample_rate: metadata.sample_rate,
            bit_depth: metadata.bit_depth,
            channels: metadata.channels,
            bits_per_coded_sample: metadata.bitrate,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AlbumSummary {
    pub title: String,
    pub album_artist: String,
    pub track_ids: Vec<TrackId>,
}

#[derive(Clone, Debug)]
pub struct ArtistSummary {
    pub name: String,
    pub track_ids: Vec<TrackId>,
}

#[derive(Clone, Debug)]
pub struct GenreSummary {
    pub name: String,
    pub track_ids: Vec<TrackId>,
}

#[derive(Clone, Debug)]
pub struct FolderSummary {
    pub path: PathBuf,
    pub name: String,
    pub display_path: String,
    pub track_ids: Vec<TrackId>,
}

#[derive(Clone, Debug, Default)]
pub struct LibrarySearchResults {
    pub tracks: Vec<TrackSummary>,
    pub albums: Vec<AlbumSummary>,
    pub artists: Vec<ArtistSummary>,
    pub genres: Vec<GenreSummary>,
}

#[derive(Clone, Debug)]
pub struct PlaybackView {
    pub current_track_id: Option<TrackId>,
    pub state: DesktopPlaybackState,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub queue_track_ids: Vec<TrackId>,
    pub current_queue_index: Option<usize>,
    pub spectrum: SpectrumSnapshot,
    pub volume: f32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LibraryScanSummary {
    pub discovered: usize,
    pub added: usize,
    pub updated: usize,
    pub failed: usize,
    pub issues: usize,
}

struct ScanResult {
    root: PathBuf,
    library: MusicLibrary,
    summary: LibraryScanSummary,
    summaries: LibrarySummaries,
}

struct LibrarySummaries {
    tracks: Vec<TrackSummary>,
    albums: Vec<AlbumSummary>,
    artists: Vec<ArtistSummary>,
    genres: Vec<GenreSummary>,
    folders: Vec<FolderSummary>,
}

/// Application facade used by the native desktop presentation.
pub struct DesktopApplication {
    library: MusicLibrary,
    tracks: Vec<TrackSummary>,
    albums: Vec<AlbumSummary>,
    artists: Vec<ArtistSummary>,
    genres: Vec<GenreSummary>,
    folders: Vec<FolderSummary>,
    player: PlayerController,
    artwork_cache: Arc<Mutex<ArtworkCache>>,
    artwork_result_rx: Option<Receiver<Result<Option<Arc<Artwork>>, String>>>,
    artwork_loading_track: Option<TrackId>,
    lyrics_cache: HashMap<TrackId, Option<Lyrics>>,
    lyrics_result_rx: Option<Receiver<Result<Option<Lyrics>, String>>>,
    lyrics_loading_track: Option<TrackId>,
    library_root: Option<PathBuf>,
    scan_result_rx: Option<Receiver<ScanResult>>,
}

impl DesktopApplication {
    pub fn new() -> Self {
        aurelis_core::initialize();
        Self {
            library: MusicLibrary::new(),
            tracks: Vec::new(),
            albums: Vec::new(),
            artists: Vec::new(),
            genres: Vec::new(),
            folders: Vec::new(),
            player: PlayerController::new(),
            artwork_cache: Arc::new(Mutex::new(ArtworkCache::new())),
            artwork_result_rx: None,
            artwork_loading_track: None,
            lyrics_cache: HashMap::new(),
            lyrics_result_rx: None,
            lyrics_loading_track: None,
            library_root: None,
            scan_result_rx: None,
        }
    }

    pub fn tracks(&self) -> &[TrackSummary] {
        &self.tracks
    }

    pub fn track(&self, track_id: TrackId) -> Option<&TrackSummary> {
        self.tracks.iter().find(|track| track.id == track_id)
    }

    pub fn albums(&self) -> &[AlbumSummary] {
        &self.albums
    }

    pub fn artists(&self) -> &[ArtistSummary] {
        &self.artists
    }

    pub fn genres(&self) -> &[GenreSummary] {
        &self.genres
    }

    pub fn folders(&self) -> &[FolderSummary] {
        &self.folders
    }

    pub fn search_tracks(&self, search: &str) -> Vec<TrackSummary> {
        let search = search.trim();
        if search.is_empty() {
            return Vec::new();
        }

        self.library
            .query(&TrackQuery::new().search(search))
            .into_iter()
            .map(TrackSummary::from)
            .collect()
    }

    pub fn search_library(&self, search: &str) -> LibrarySearchResults {
        let search = search.trim();
        if search.is_empty() {
            return LibrarySearchResults::default();
        }

        let tracks = self.search_tracks(search);
        let matching_ids: HashSet<_> = tracks.iter().map(|track| track.id).collect();
        let normalized_search = search.to_lowercase();
        let has_matching_track = |track_ids: &[TrackId]| {
            track_ids
                .iter()
                .any(|track_id| matching_ids.contains(track_id))
        };

        LibrarySearchResults {
            tracks,
            albums: self
                .albums
                .iter()
                .filter(|album| {
                    album.title.to_lowercase().contains(&normalized_search)
                        || album
                            .album_artist
                            .to_lowercase()
                            .contains(&normalized_search)
                        || has_matching_track(&album.track_ids)
                })
                .cloned()
                .collect(),
            artists: self
                .artists
                .iter()
                .filter(|artist| {
                    artist.name.to_lowercase().contains(&normalized_search)
                        || has_matching_track(&artist.track_ids)
                })
                .cloned()
                .collect(),
            genres: self
                .genres
                .iter()
                .filter(|genre| {
                    genre.name.to_lowercase().contains(&normalized_search)
                        || has_matching_track(&genre.track_ids)
                })
                .cloned()
                .collect(),
        }
    }

    pub fn library_root(&self) -> Option<&Path> {
        self.library_root.as_deref()
    }

    pub fn is_scanning(&self) -> bool {
        self.scan_result_rx.is_some()
    }

    /// Start one cached directory scan on a worker thread.
    pub fn begin_scan(&mut self, root: impl Into<PathBuf>) -> Result<(), String> {
        if self.is_scanning() {
            return Err("A library scan is already in progress.".to_owned());
        }

        let root = root.into();
        let worker_root = root.clone();
        let (result_tx, result_rx) = mpsc::channel();

        thread::Builder::new()
            .name("aurelis-library-scan".to_owned())
            .spawn(move || {
                let mut library = MusicLibrary::new();
                let report = library.scan_directory_cached(&worker_root);
                let summary = LibraryScanSummary {
                    discovered: report.discovered,
                    added: report.added,
                    updated: report.updated,
                    failed: report.failed,
                    issues: report.issues.len(),
                };
                let summaries = build_library_summaries(&library);
                let _ = result_tx.send(ScanResult {
                    root: worker_root,
                    library,
                    summary,
                    summaries,
                });
            })
            .map_err(|error| format!("Could not start the library scan: {error}"))?;

        self.scan_result_rx = Some(result_rx);
        Ok(())
    }

    /// Apply a completed scan on the application thread.
    pub fn poll_library_scan(&mut self) -> Option<Result<LibraryScanSummary, String>> {
        let received = match self.scan_result_rx.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.scan_result_rx = None;
                return Some(Err("The library scan ended unexpectedly.".to_owned()));
            }
        };
        self.scan_result_rx = None;

        if let Err(error) = self.player.shutdown() {
            return Some(Err(format!(
                "Could not stop playback before replacing the library: {error}"
            )));
        }

        self.player = PlayerController::new();
        self.library = received.library;
        self.tracks = received.summaries.tracks;
        self.albums = received.summaries.albums;
        self.artists = received.summaries.artists;
        self.genres = received.summaries.genres;
        self.folders = received.summaries.folders;
        self.library_root = Some(received.root);
        self.artwork_result_rx = None;
        self.artwork_loading_track = None;
        self.artwork_cache = Arc::new(Mutex::new(ArtworkCache::new()));
        self.lyrics_result_rx = None;
        self.lyrics_loading_track = None;
        self.lyrics_cache.clear();

        Some(Ok(received.summary))
    }

    /// Poll playback completion and return any playback error for presentation.
    pub fn poll_playback(&mut self) -> Option<String> {
        self.player
            .poll_playback(&self.library)
            .and_then(Result::err)
            .map(|error| error.to_string())
    }

    pub fn playback(&self) -> PlaybackView {
        let snapshot = self.player.snapshot(&self.library);
        let state = match snapshot.state() {
            PlaybackStatus::Stopped => DesktopPlaybackState::Stopped,
            PlaybackStatus::Playing => DesktopPlaybackState::Playing,
            PlaybackStatus::Paused => DesktopPlaybackState::Paused,
            PlaybackStatus::Finished => DesktopPlaybackState::Finished,
        };

        PlaybackView {
            current_track_id: snapshot.current_track_id(),
            state,
            position: snapshot.position(),
            duration: snapshot.duration(),
            queue_track_ids: snapshot.queue().track_ids().to_vec(),
            current_queue_index: snapshot.queue().current_index(),
            spectrum: self.player.spectrum_snapshot(),
            volume: self.player.volume(),
        }
    }

    pub fn play_track(&mut self, track_id: TrackId) -> Result<(), String> {
        self.player
            .play_track(&self.library, track_id)
            .map_err(|error| error.to_string())
    }

    pub fn enqueue_track(&mut self, track_id: TrackId) -> Result<(), String> {
        self.player
            .enqueue(&self.library, track_id)
            .map_err(|error| error.to_string())
    }

    pub fn enqueue_tracks(
        &mut self,
        track_ids: impl IntoIterator<Item = TrackId>,
    ) -> Result<(), String> {
        self.player
            .enqueue_tracks(&self.library, track_ids)
            .map_err(|error| error.to_string())
    }

    pub fn remove_upcoming_track(&mut self, index: usize) -> Result<TrackId, String> {
        self.player
            .remove_upcoming_track(index)
            .ok_or_else(|| "Only upcoming tracks can be removed from the queue.".to_owned())
    }

    pub fn move_upcoming_track(&mut self, from: usize, to: usize) -> Result<(), String> {
        self.player
            .move_upcoming_track(from, to)
            .then_some(())
            .ok_or_else(|| "Only upcoming tracks can be reordered in the queue.".to_owned())
    }

    /// Clear upcoming entries while keeping the active queue item and playback intact.
    pub fn clear_upcoming_queue(&mut self) -> usize {
        self.player.clear_upcoming_queue()
    }

    pub fn toggle_playback(&mut self) -> Result<(), String> {
        match self.player.playback_state() {
            PlaybackStatus::Playing => {
                self.player.pause();
                Ok(())
            }
            PlaybackStatus::Paused => {
                self.player.resume();
                Ok(())
            }
            PlaybackStatus::Stopped | PlaybackStatus::Finished => {
                let Some(track_id) = self.player.current_track_id() else {
                    return Err("Select a track before starting playback.".to_owned());
                };
                self.play_track(track_id)
            }
        }
    }

    pub fn stop(&mut self) {
        self.player.stop();
    }

    /// Set the application-wide output volume for current and future playback.
    pub fn set_volume(&mut self, volume: f32) {
        self.player.set_volume(volume);
    }

    pub fn play_next(&mut self) -> Result<Option<TrackId>, String> {
        self.player
            .next(&self.library)
            .map_err(|error| error.to_string())
    }

    pub fn play_previous(&mut self) -> Result<Option<TrackId>, String> {
        self.player
            .previous(&self.library)
            .map_err(|error| error.to_string())
    }

    /// Seek to an absolute position using the controller's existing
    /// second-based seek operation.
    pub fn seek_to(&mut self, position: Duration) -> Result<(), String> {
        let duration = self.player.snapshot(&self.library).duration();
        let position = duration.map_or(position, |duration| position.min(duration));
        let current_seconds = self.player.position().as_secs().min(i64::MAX as u64) as i64;
        let target_seconds = position.as_secs().min(i64::MAX as u64) as i64;
        self.player
            .seek_by_seconds(target_seconds.saturating_sub(current_seconds))
            .map_err(|error| error.to_string())
    }

    pub fn artwork_loading_track(&self) -> Option<TrackId> {
        self.artwork_loading_track
    }

    /// Load artwork through the existing cache without blocking the UI thread.
    pub fn request_artwork(&mut self, track_id: TrackId) -> Result<(), String> {
        if self.artwork_loading_track.is_some() {
            if self.artwork_loading_track == Some(track_id) {
                return Ok(());
            }
            return Err("Another artwork lookup is already in progress.".to_owned());
        }

        let track = self
            .library
            .get(track_id)
            .cloned()
            .ok_or_else(|| "The selected track is no longer in the library.".to_owned())?;
        let artwork_cache = Arc::clone(&self.artwork_cache);
        let (result_tx, result_rx) = mpsc::channel();

        thread::Builder::new()
            .name("aurelis-artwork-lookup".to_owned())
            .spawn(move || {
                let artwork = artwork_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_for_track(&track);
                let _ = result_tx.send(Ok(artwork));
            })
            .map_err(|error| format!("Could not start artwork lookup: {error}"))?;

        self.artwork_result_rx = Some(result_rx);
        self.artwork_loading_track = Some(track_id);
        Ok(())
    }

    pub fn poll_artwork(&mut self) -> Option<(TrackId, Result<Option<ArtworkView>, String>)> {
        let track_id = self.artwork_loading_track?;
        let result = match self.artwork_result_rx.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.artwork_result_rx = None;
                self.artwork_loading_track = None;
                return Some((
                    track_id,
                    Err("Artwork lookup ended unexpectedly.".to_owned()),
                ));
            }
        };
        self.artwork_result_rx = None;
        self.artwork_loading_track = None;
        Some((
            track_id,
            result.map(|artwork| artwork.map(|artwork| ArtworkView::from(artwork.as_ref()))),
        ))
    }

    pub fn lyrics_loading_track(&self) -> Option<TrackId> {
        self.lyrics_loading_track
    }

    /// Load a track's local lyrics sidecar without blocking the UI thread.
    pub fn request_lyrics(&mut self, track_id: TrackId) -> Result<(), String> {
        if self.lyrics_loading_track.is_some() {
            if self.lyrics_loading_track == Some(track_id) {
                return Ok(());
            }
            return Err("Another lyrics lookup is already in progress.".to_owned());
        }

        let (result_tx, result_rx) = mpsc::channel();
        if let Some(cached) = self.lyrics_cache.get(&track_id) {
            let _ = result_tx.send(Ok(cached.clone()));
            self.lyrics_result_rx = Some(result_rx);
            self.lyrics_loading_track = Some(track_id);
            return Ok(());
        }

        let track = self
            .library
            .get(track_id)
            .cloned()
            .ok_or_else(|| "The selected track is no longer in the library.".to_owned())?;

        thread::Builder::new()
            .name("aurelis-lyrics-lookup".to_owned())
            .spawn(move || {
                let lyrics = load_lyrics_for_track(&track).map_err(|error| error.to_string());
                let _ = result_tx.send(lyrics);
            })
            .map_err(|error| format!("Could not start lyrics lookup: {error}"))?;

        self.lyrics_result_rx = Some(result_rx);
        self.lyrics_loading_track = Some(track_id);
        Ok(())
    }

    pub fn poll_lyrics(&mut self) -> Option<(TrackId, Result<Option<Lyrics>, String>)> {
        let track_id = self.lyrics_loading_track?;
        let result = match self.lyrics_result_rx.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.lyrics_result_rx = None;
                self.lyrics_loading_track = None;
                return Some((
                    track_id,
                    Err("Lyrics lookup ended unexpectedly.".to_owned()),
                ));
            }
        };
        self.lyrics_result_rx = None;
        self.lyrics_loading_track = None;
        if let Ok(lyrics) = &result {
            if self.lyrics_cache.len() >= MAX_LYRICS_CACHE_ENTRIES
                && !self.lyrics_cache.contains_key(&track_id)
                && let Some(evicted_track_id) = self.lyrics_cache.keys().next().copied()
            {
                self.lyrics_cache.remove(&evicted_track_id);
            }
            self.lyrics_cache.insert(track_id, lyrics.clone());
        }
        Some((track_id, result))
    }
}

fn build_library_summaries(library: &MusicLibrary) -> LibrarySummaries {
    let tracks: Vec<_> = library.iter().map(TrackSummary::from).collect();
    let albums = library
        .albums()
        .into_iter()
        .map(|album: Album| AlbumSummary {
            title: album.title().to_owned(),
            album_artist: album.album_artist().to_owned(),
            track_ids: album.track_ids().to_vec(),
        })
        .collect();
    let artists = library
        .artists()
        .into_iter()
        .map(|artist: Artist| ArtistSummary {
            name: artist.name().to_owned(),
            track_ids: artist.track_ids().to_vec(),
        })
        .collect();
    let genres = summarize_genres(&tracks);
    let folders = summarize_folders(&tracks);

    LibrarySummaries {
        tracks,
        albums,
        artists,
        genres,
        folders,
    }
}

fn summarize_genres(tracks: &[TrackSummary]) -> Vec<GenreSummary> {
    let mut groups: BTreeMap<String, (String, Vec<&TrackSummary>)> = BTreeMap::new();

    for track in tracks {
        let Some(genre) = track
            .genre
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let key = genre.to_lowercase();
        let group = groups
            .entry(key)
            .or_insert_with(|| (genre.to_owned(), Vec::new()));
        if genre < group.0.as_str() {
            group.0 = genre.to_owned();
        }
        group.1.push(track);
    }

    groups
        .into_values()
        .map(|(name, mut tracks)| {
            tracks
                .sort_by(|left, right| left.title.to_lowercase().cmp(&right.title.to_lowercase()));
            GenreSummary {
                name,
                track_ids: tracks.iter().map(|track| track.id).collect(),
            }
        })
        .collect()
}

fn summarize_folders(tracks: &[TrackSummary]) -> Vec<FolderSummary> {
    let mut groups: BTreeMap<PathBuf, Vec<&TrackSummary>> = BTreeMap::new();

    for track in tracks {
        let folder = track
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| track.path.clone());
        groups.entry(folder).or_default().push(track);
    }

    groups
        .into_iter()
        .map(|(path, mut tracks)| {
            tracks
                .sort_by(|left, right| left.title.to_lowercase().cmp(&right.title.to_lowercase()));
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            let display_path = path.display().to_string();
            FolderSummary {
                path,
                name,
                display_path,
                track_ids: tracks.iter().map(|track| track.id).collect(),
            }
        })
        .collect()
}
impl Default for DesktopApplication {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DesktopApplication {
    fn drop(&mut self) {
        let _ = self.player.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::Instant,
    };

    use super::{DesktopApplication, TrackSummary, build_library_summaries};
    use aurelis_core::audio::AudioMetadata;
    use aurelis_core::library::{Lyrics, MusicLibrary, TrackId};

    static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "aurelis-desktop-app-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create test music directory");
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

    fn write_test_wav(path: &Path) {
        let samples = [0_u8; 16];
        let data_len = samples.len() as u32;
        let sample_rate = 44_100_u32;
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
        fs::write(path, bytes).expect("write test WAV");
    }

    #[test]
    fn background_cached_scan_populates_real_track_summaries() {
        let directory = TemporaryDirectory::new();
        write_test_wav(&directory.path().join("Sample Song.wav"));

        let mut application = DesktopApplication::new();
        application
            .begin_scan(directory.path())
            .expect("start library scan");
        assert!(application.is_scanning());
        assert!(application.begin_scan(directory.path()).is_err());

        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let summary = loop {
            if let Some(result) = application.poll_library_scan() {
                break result.expect("apply completed library scan");
            }
            assert!(
                Instant::now() < deadline,
                "library scan did not complete before the deadline"
            );
            std::thread::yield_now();
        };

        assert_eq!(summary.discovered, 1);
        assert_eq!(summary.added, 1);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.issues, 0);
        assert_eq!(application.tracks().len(), 1);
        assert_eq!(application.tracks()[0].title, "Sample Song");
        assert_eq!(application.tracks()[0].codec, "PCM");
        assert_eq!(application.library_root(), Some(directory.path()));
        assert!(!application.is_scanning());
    }

    fn wait_for_lyrics(
        application: &mut DesktopApplication,
        expected_id: TrackId,
    ) -> Option<Lyrics> {
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some((track_id, result)) = application.poll_lyrics() {
                assert_eq!(track_id, expected_id);
                return result.expect("load local lyrics");
            }
            assert!(
                Instant::now() < deadline,
                "lyrics lookup did not complete before the deadline"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn asynchronously_loads_and_caches_local_synced_lyrics() {
        let directory = TemporaryDirectory::new();
        let track_path = directory.path().join("Sample Song.flac");
        fs::write(
            track_path.with_extension("lrc"),
            "[00:01.00]First line\n[00:02.50]Second line",
        )
        .expect("write LRC sidecar");

        let mut application = DesktopApplication::new();
        let track_id = application
            .library
            .add_track(track_path, AudioMetadata::default());

        application
            .request_lyrics(track_id)
            .expect("start lyrics lookup");
        assert_eq!(application.lyrics_loading_track(), Some(track_id));
        let lyrics = wait_for_lyrics(&mut application, track_id).expect("synced lyrics");
        let lines = lyrics.synced_lines().expect("timestamped lines");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].timestamp(), std::time::Duration::from_secs(1));
        assert_eq!(lines[1].text(), "Second line");

        application
            .request_lyrics(track_id)
            .expect("request cached lyrics");
        assert_eq!(wait_for_lyrics(&mut application, track_id), Some(lyrics));
    }

    #[test]
    fn reports_missing_local_lyrics_without_error() {
        let directory = TemporaryDirectory::new();
        let mut application = DesktopApplication::new();
        let track_id = application.library.add_track(
            directory.path().join("No Lyrics.flac"),
            AudioMetadata::default(),
        );

        application
            .request_lyrics(track_id)
            .expect("start lyrics lookup");

        assert!(wait_for_lyrics(&mut application, track_id).is_none());
    }

    #[test]
    fn desktop_queue_facade_enqueues_reorders_removes_and_clears_upcoming_tracks() {
        let mut application = DesktopApplication::new();
        let first = application
            .library
            .add_track("Music/First.flac", AudioMetadata::default());
        let second = application
            .library
            .add_track("Music/Second.flac", AudioMetadata::default());
        let third = application
            .library
            .add_track("Music/Third.flac", AudioMetadata::default());

        application
            .enqueue_tracks([first, second, third])
            .expect("enqueue tracks");
        assert_eq!(
            application.playback().queue_track_ids,
            [first, second, third]
        );

        application
            .move_upcoming_track(0, 2)
            .expect("reorder queued tracks");
        assert_eq!(
            application.playback().queue_track_ids,
            [second, third, first]
        );
        assert_eq!(
            application
                .remove_upcoming_track(1)
                .expect("remove upcoming track"),
            third
        );
        assert_eq!(application.clear_upcoming_queue(), 2);
        assert!(application.playback().queue_track_ids.is_empty());
    }

    #[test]
    fn track_summary_preserves_real_descriptive_tags() {
        let mut library = MusicLibrary::new();
        let id = library.add_track(
            "music/tagged.flac",
            aurelis_core::audio::AudioMetadata {
                genre: Some("Trip hop".to_owned()),
                year: Some("1998".to_owned()),
                track: Some(4),
                disc: Some(2),
                ..Default::default()
            },
        );
        let track = library.get(id).expect("inserted track");
        let summary = TrackSummary::from(track);

        assert_eq!(summary.genre.as_deref(), Some("Trip hop"));
        assert_eq!(summary.year.as_deref(), Some("1998"));
        assert_eq!(summary.track_number, Some(4));
        assert_eq!(summary.disc_number, Some(2));
    }
    #[test]
    fn desktop_search_uses_core_metadata_search() {
        let mut application = DesktopApplication::new();
        let matching = application.library.add_track(
            "Music/Blue Train.flac",
            AudioMetadata {
                title: Some("Blue Train".to_owned()),
                artist: Some("John Coltrane".to_owned()),
                album: Some("Blue Train Album".to_owned()),
                genre: Some("Jazz".to_owned()),
                ..AudioMetadata::default()
            },
        );
        application.library.add_track(
            "Music/Kind of Blue.flac",
            AudioMetadata {
                title: Some("Kind of Blue".to_owned()),
                artist: Some("Miles Davis".to_owned()),
                genre: Some("Jazz".to_owned()),
                ..AudioMetadata::default()
            },
        );

        let summaries = build_library_summaries(&application.library);
        application.tracks = summaries.tracks;
        application.albums = summaries.albums;
        application.artists = summaries.artists;
        application.genres = summaries.genres;
        application.folders = summaries.folders;

        let results = application.search_library("COLTRANE");

        assert_eq!(results.tracks.len(), 1);
        assert_eq!(results.tracks[0].id, matching);
        assert_eq!(results.albums.len(), 1);
        assert_eq!(results.albums[0].title, "Blue Train Album");
        assert_eq!(results.artists.len(), 1);
        assert_eq!(results.artists[0].name, "John Coltrane");
        assert_eq!(results.genres.len(), 1);
        assert_eq!(results.genres[0].name, "Jazz");
    }

    #[test]
    fn background_library_summaries_group_real_album_artist_genre_and_folder_data() {
        let mut library = MusicLibrary::new();
        let second_track = library.add_track(
            "Music/Album/02.flac",
            AudioMetadata {
                title: Some("Second".to_owned()),
                artist: Some("Singer B".to_owned()),
                album: Some("Compilation".to_owned()),
                album_artist: Some("Various Artists".to_owned()),
                genre: Some("Jazz".to_owned()),
                track: Some(2),
                ..AudioMetadata::default()
            },
        );
        let first_track = library.add_track(
            "Music/Album/01.flac",
            AudioMetadata {
                title: Some("First".to_owned()),
                artist: Some("Singer A".to_owned()),
                album: Some("Compilation".to_owned()),
                album_artist: Some("Various Artists".to_owned()),
                genre: Some(" jazz ".to_owned()),
                track: Some(1),
                ..AudioMetadata::default()
            },
        );
        library.add_track(
            "Music/Other/Track.flac",
            AudioMetadata {
                title: Some("Other".to_owned()),
                artist: Some("Singer C".to_owned()),
                album: Some("Other Album".to_owned()),
                genre: Some("Ambient".to_owned()),
                ..AudioMetadata::default()
            },
        );

        let summaries = build_library_summaries(&library);
        let album = summaries
            .albums
            .iter()
            .find(|album| album.title == "Compilation")
            .expect("compilation summary");

        assert_eq!(album.album_artist, "Various Artists");
        assert_eq!(album.track_ids, [first_track, second_track]);
        assert_eq!(summaries.artists.len(), 3);
        assert_eq!(summaries.genres.len(), 2);
        assert_eq!(summaries.folders.len(), 2);
    }
}
