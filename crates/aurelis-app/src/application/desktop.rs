use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use aurelis_core::audio::SpectrumSnapshot;
use aurelis_core::library::{
    Album, Artist, Artwork, ArtworkCache, ArtworkFormat, ArtworkSource, ListeningAnalytics,
    ListeningHistory, ListeningPlay, Lyrics, MusicLibrary, PlaylistCollection, PlaylistError,
    PlaylistId, Track, TrackId, TrackQuery, load_lyrics_for_track,
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
            bits_per_coded_sample: metadata.bits_per_coded_sample,
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

#[derive(Clone, Debug)]
pub struct PlaylistSummary {
    pub id: PlaylistId,
    pub name: String,
    pub track_count: usize,
}

#[derive(Clone, Debug)]
pub struct PlaylistTrackSummary {
    pub path: PathBuf,
    pub track: Option<TrackSummary>,
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
    playlists: PlaylistCollection,
    playlist_error: Option<String>,
    listening_history: ListeningHistory,
    listening_history_error: Option<String>,
}

enum ListeningHistoryCommand {
    SelectLibrary {
        epoch: u64,
        path: PathBuf,
        history: Option<ListeningHistory>,
    },
    Record {
        epoch: u64,
        play: ListeningPlay,
    },
}

struct ListeningHistoryWriteResult {
    epoch: u64,
    result: Result<(), String>,
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
    playlists: PlaylistCollection,
    playlist_path: Option<PathBuf>,
    playlist_error: Option<String>,
    listening_history: ListeningHistory,
    listening_analytics: ListeningAnalytics,
    listening_history_error: Option<String>,
    listening_history_writable: bool,
    listening_history_epoch: u64,
    listening_history_writer: Option<Sender<ListeningHistoryCommand>>,
    listening_history_result_rx: Option<Receiver<ListeningHistoryWriteResult>>,
    listening_history_writer_thread: Option<JoinHandle<()>>,
    observed_session_generation: Option<u64>,
}

impl DesktopApplication {
    pub fn new() -> Self {
        aurelis_core::initialize();
        let (
            listening_history_writer,
            listening_history_result_rx,
            listening_history_writer_thread,
            listening_history_error,
        ) = match start_listening_history_writer() {
            Ok((writer, result_rx, thread)) => (Some(writer), Some(result_rx), Some(thread), None),
            Err(error) => (None, None, None, Some(error)),
        };
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
            playlists: PlaylistCollection::new(),
            playlist_path: None,
            playlist_error: None,
            listening_history: ListeningHistory::default(),
            listening_analytics: ListeningAnalytics::default(),
            listening_history_error,
            listening_history_writable: false,
            listening_history_epoch: 0,
            listening_history_writer,
            listening_history_result_rx,
            listening_history_writer_thread,
            observed_session_generation: None,
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

    pub fn playlists(&self) -> Vec<PlaylistSummary> {
        self.playlists
            .iter()
            .map(|playlist| PlaylistSummary {
                id: playlist.id(),
                name: playlist.name().to_owned(),
                track_count: playlist.len(),
            })
            .collect()
    }

    pub fn playlist_tracks(
        &self,
        playlist_id: PlaylistId,
    ) -> Result<Vec<PlaylistTrackSummary>, String> {
        let playlist = self
            .playlists
            .get(playlist_id)
            .ok_or_else(|| "The selected playlist is no longer available.".to_owned())?;
        Ok(playlist
            .tracks()
            .iter()
            .map(|entry| {
                let path = entry.path().to_path_buf();
                let track = self.library.get_by_path(&path).map(TrackSummary::from);
                PlaylistTrackSummary { path, track }
            })
            .collect())
    }

    pub fn playlist_storage_error(&self) -> Option<&str> {
        self.playlist_error.as_deref()
    }

    pub fn listening_analytics(&self) -> &ListeningAnalytics {
        &self.listening_analytics
    }

    pub fn listening_history_storage_error(&self) -> Option<&str> {
        self.listening_history_error.as_deref()
    }

    pub fn can_edit_playlists(&self) -> bool {
        self.playlist_path.is_some() && self.playlist_error.is_none()
    }

    pub fn create_playlist(&mut self, name: &str) -> Result<PlaylistId, String> {
        self.update_playlists(|playlists| playlists.create_playlist(name))
    }

    pub fn rename_playlist(&mut self, id: PlaylistId, name: &str) -> Result<(), String> {
        self.update_playlists(|playlists| playlists.rename_playlist(id, name))
    }

    pub fn delete_playlist(&mut self, id: PlaylistId) -> Result<(), String> {
        self.update_playlists(|playlists| playlists.delete_playlist(id).map(|_| ()))
    }

    pub fn add_track_to_playlist(
        &mut self,
        playlist_id: PlaylistId,
        track_id: TrackId,
    ) -> Result<(), String> {
        let track = self
            .library
            .get(track_id)
            .cloned()
            .ok_or_else(|| "The selected track is no longer in the library.".to_owned())?;
        self.update_playlists(|playlists| playlists.add_track(playlist_id, &track))
    }

    pub fn remove_playlist_track(
        &mut self,
        playlist_id: PlaylistId,
        index: usize,
    ) -> Result<(), String> {
        self.update_playlists(|playlists| playlists.remove_track(playlist_id, index).map(|_| ()))
    }

    pub fn move_playlist_track(
        &mut self,
        playlist_id: PlaylistId,
        from: usize,
        to: usize,
    ) -> Result<(), String> {
        self.update_playlists(|playlists| playlists.move_track(playlist_id, from, to))
    }

    pub fn queue_playlist(&mut self, playlist_id: PlaylistId) -> Result<(), String> {
        let track_ids = self
            .playlists
            .resolve_track_ids(playlist_id, &self.library)
            .map_err(|error| error.to_string())?;
        if track_ids.is_empty() {
            return Err("This playlist has no tracks to queue.".to_owned());
        }
        self.player
            .enqueue_tracks(&self.library, track_ids)
            .map_err(|error| error.to_string())
    }

    pub fn play_playlist(&mut self, playlist_id: PlaylistId) -> Result<(), String> {
        let track_ids = self
            .playlists
            .resolve_track_ids(playlist_id, &self.library)
            .map_err(|error| error.to_string())?;
        let Some((&first, remaining)) = track_ids.split_first() else {
            return Err("This playlist has no tracks to play.".to_owned());
        };

        self.player
            .play_track(&self.library, first)
            .map_err(|error| error.to_string())?;
        self.record_new_playback_session();
        self.player.clear_upcoming_queue();
        self.player
            .enqueue_tracks(&self.library, remaining.iter().copied())
            .map_err(|error| error.to_string())
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
                let playlist_path = worker_root.join(".aurelis_cache").join("playlists.json");
                let (playlists, playlist_error) = load_playlists(&playlist_path);
                let listening_history_path = worker_root
                    .join(".aurelis_cache")
                    .join("listening-history.json");
                let (listening_history, listening_history_error) =
                    load_listening_history(&listening_history_path);
                let _ = result_tx.send(ScanResult {
                    root: worker_root,
                    library,
                    summary,
                    summaries,
                    playlists,
                    playlist_error,
                    listening_history,
                    listening_history_error,
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

        let volume = self.player.volume();
        if let Err(error) = self.player.shutdown() {
            return Some(Err(format!(
                "Could not stop playback before replacing the library: {error}"
            )));
        }

        let same_library_root = self.library_root.as_deref() == Some(received.root.as_path());
        let previous_history_error = self.listening_history_error.clone();
        let previous_history_writable = self.listening_history_writable;
        self.player = PlayerController::new();
        self.player.set_volume(volume);
        self.library = received.library;
        self.tracks = received.summaries.tracks;
        self.albums = received.summaries.albums;
        self.artists = received.summaries.artists;
        self.genres = received.summaries.genres;
        self.folders = received.summaries.folders;
        self.library_root = Some(received.root.clone());
        self.playlist_path = Some(received.root.join(".aurelis_cache").join("playlists.json"));
        self.playlists = received.playlists;
        self.playlist_error = received.playlist_error;
        self.listening_history = if same_library_root {
            self.listening_history.clone()
        } else {
            received.listening_history
        };
        self.listening_analytics = self.listening_history.analytics(8, 5);
        let history_path = received
            .root
            .join(".aurelis_cache")
            .join("listening-history.json");
        self.listening_history_error = if same_library_root {
            previous_history_error
        } else {
            received.listening_history_error
        };
        self.listening_history_writable = if same_library_root {
            previous_history_writable
        } else {
            self.listening_history_error.is_none()
        };
        self.observed_session_generation = None;
        self.listening_history_epoch = self.listening_history_epoch.wrapping_add(1);
        if let Some(writer) = &self.listening_history_writer {
            let history = self
                .listening_history_writable
                .then(|| self.listening_history.clone());
            if writer
                .send(ListeningHistoryCommand::SelectLibrary {
                    epoch: self.listening_history_epoch,
                    path: history_path,
                    history,
                })
                .is_err()
            {
                self.listening_history_writable = false;
                self.listening_history_error =
                    Some("The local listening history writer stopped unexpectedly.".to_owned());
            }
        } else {
            self.listening_history_writable = false;
            if self.listening_history_error.is_none() {
                self.listening_history_error =
                    Some("The local listening history writer is unavailable.".to_owned());
            }
        }
        self.artwork_result_rx = None;
        self.artwork_loading_track = None;
        self.artwork_cache = Arc::new(Mutex::new(ArtworkCache::new()));
        self.lyrics_result_rx = None;
        self.lyrics_loading_track = None;
        self.lyrics_cache.clear();

        Some(Ok(received.summary))
    }

    fn record_new_playback_session(&mut self) {
        let generation = self.player.session_generation();
        if self.observed_session_generation == Some(generation) {
            return;
        }
        self.observed_session_generation = Some(generation);

        let Some(track_id) = self.player.current_track_id() else {
            return;
        };
        self.record_track_start(track_id);
    }

    fn record_track_start(&mut self, track_id: TrackId) {
        let Some(track) = self.library.get(track_id) else {
            return;
        };
        let play = ListeningPlay {
            path: track.path().to_path_buf(),
            title: track.display_title().into_owned(),
            artist: track.display_artist().into_owned(),
            album: track.display_album().into_owned(),
            played_at_unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };

        if let Err(error) = self.listening_history.record_play(play.clone()) {
            self.listening_history_error = Some(format!(
                "Could not record this playback in listening history: {error}"
            ));
            return;
        }
        self.listening_analytics = self.listening_history.analytics(8, 5);

        if !self.listening_history_writable {
            return;
        }
        let Some(writer) = &self.listening_history_writer else {
            self.listening_history_writable = false;
            self.listening_history_error =
                Some("The local listening history writer is unavailable.".to_owned());
            return;
        };
        if writer
            .send(ListeningHistoryCommand::Record {
                epoch: self.listening_history_epoch,
                play,
            })
            .is_err()
        {
            self.listening_history_writable = false;
            self.listening_history_error =
                Some("The local listening history writer stopped unexpectedly.".to_owned());
        }
    }

    fn poll_listening_history_writer(&mut self) {
        let Some(result_rx) = &self.listening_history_result_rx else {
            return;
        };
        let mut latest_result = None;
        loop {
            match result_rx.try_recv() {
                Ok(result) if result.epoch == self.listening_history_epoch => {
                    latest_result = Some(result.result);
                }
                Ok(_) => {}
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        match latest_result {
            Some(Ok(())) => self.listening_history_error = None,
            Some(Err(error)) => self.listening_history_error = Some(error),
            None => {}
        }
    }

    fn update_playlists<T>(
        &mut self,
        update: impl FnOnce(&mut PlaylistCollection) -> Result<T, PlaylistError>,
    ) -> Result<T, String> {
        let path = self
            .playlist_path
            .clone()
            .ok_or_else(|| "Scan a music folder before editing playlists.".to_owned())?;
        if let Some(error) = &self.playlist_error {
            return Err(format!(
                "Saved playlists could not be loaded and are protected from being overwritten: {error}"
            ));
        }

        let mut updated = self.playlists.clone();
        let result = update(&mut updated).map_err(|error| error.to_string())?;
        let parent = path
            .parent()
            .ok_or_else(|| "The playlist storage location is invalid.".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Could not create playlist storage at {}: {error}",
                parent.display()
            )
        })?;
        updated
            .save_to_path(&path)
            .map_err(|error| format!("Could not save playlists: {error}"))?;
        self.playlists = updated;
        Ok(result)
    }

    /// Poll playback completion and return any playback error for presentation.
    pub fn poll_playback(&mut self) -> Option<String> {
        let error = self
            .player
            .poll_playback(&self.library)
            .and_then(Result::err)
            .map(|error| error.to_string());
        self.record_new_playback_session();
        self.poll_listening_history_writer();
        error
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
            .map_err(|error| error.to_string())?;
        self.record_new_playback_session();
        Ok(())
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
        let result = self
            .player
            .next(&self.library)
            .map_err(|error| error.to_string())?;
        self.record_new_playback_session();
        Ok(result)
    }

    pub fn play_previous(&mut self) -> Result<Option<TrackId>, String> {
        let result = self
            .player
            .previous(&self.library)
            .map_err(|error| error.to_string())?;
        self.record_new_playback_session();
        Ok(result)
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
        self.listening_history_writer.take();
        if let Some(writer_thread) = self.listening_history_writer_thread.take() {
            let _ = writer_thread.join();
        }
    }
}

fn start_listening_history_writer() -> Result<
    (
        Sender<ListeningHistoryCommand>,
        Receiver<ListeningHistoryWriteResult>,
        JoinHandle<()>,
    ),
    String,
> {
    let (command_tx, command_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let worker = thread::Builder::new()
        .name("aurelis-listening-history".to_owned())
        .spawn(move || {
            let mut selected_library: Option<(u64, PathBuf, ListeningHistory)> = None;
            while let Ok(command) = command_rx.recv() {
                match command {
                    ListeningHistoryCommand::SelectLibrary {
                        epoch,
                        path,
                        history,
                    } => {
                        selected_library = history.map(|history| (epoch, path, history));
                    }
                    ListeningHistoryCommand::Record { epoch, play } => {
                        let Some((selected_epoch, path, history)) = selected_library.as_mut()
                        else {
                            continue;
                        };
                        if *selected_epoch != epoch {
                            continue;
                        }

                        let path_display = path.display().to_string();
                        let result = history
                            .record_play(play)
                            .and_then(|()| history.save_to_path(path.as_path()))
                            .map_err(|error| {
                                format!(
                                    "Could not save listening history to {}: {error}",
                                    path_display
                                )
                            });
                        let _ = result_tx.send(ListeningHistoryWriteResult { epoch, result });
                    }
                }
            }
        })
        .map_err(|error| format!("Could not start local listening history storage: {error}"))?;
    Ok((command_tx, result_rx, worker))
}

fn load_playlists(path: &Path) -> (PlaylistCollection, Option<String>) {
    match PlaylistCollection::load_from_path(path) {
        Ok(playlists) => (playlists, None),
        Err(PlaylistError::Io { source, .. }) if source.kind() == ErrorKind::NotFound => {
            (PlaylistCollection::new(), None)
        }
        Err(error) => (
            PlaylistCollection::new(),
            Some(format!(
                "Could not load playlists from {}: {error}",
                path.display()
            )),
        ),
    }
}

fn load_listening_history(path: &Path) -> (ListeningHistory, Option<String>) {
    match ListeningHistory::load_from_path(path) {
        Ok(history) => (history, None),
        Err(error) if error.kind() == ErrorKind::NotFound => (ListeningHistory::default(), None),
        Err(error) => (
            ListeningHistory::default(),
            Some(format!(
                "Could not load listening history from {}: {error}",
                path.display()
            )),
        ),
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

    use super::{DesktopApplication, LibraryScanSummary, TrackSummary, build_library_summaries};
    use aurelis_core::audio::AudioMetadata;
    use aurelis_core::library::{ListeningHistory, ListeningPlay, Lyrics, MusicLibrary, TrackId};

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

    fn wait_for_scan(application: &mut DesktopApplication) -> LibraryScanSummary {
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(result) = application.poll_library_scan() {
                return result.expect("apply completed library scan");
            }
            assert!(
                Instant::now() < deadline,
                "library scan did not complete before the deadline"
            );
            std::thread::yield_now();
        }
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

        let summary = wait_for_scan(&mut application);

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

    #[test]
    fn library_scan_preserves_the_session_output_volume() {
        let directory = TemporaryDirectory::new();
        write_test_wav(&directory.path().join("Sample Song.wav"));

        let mut application = DesktopApplication::new();
        application.set_volume(0.37);
        application
            .begin_scan(directory.path())
            .expect("start library scan");
        wait_for_scan(&mut application);

        assert_eq!(application.playback().volume, 0.37);
    }

    #[test]
    fn desktop_playlists_persist_order_and_keep_missing_tracks_visible() {
        let directory = TemporaryDirectory::new();
        write_test_wav(&directory.path().join("First.wav"));
        write_test_wav(&directory.path().join("Second.wav"));

        let mut application = DesktopApplication::new();
        application
            .begin_scan(directory.path())
            .expect("start library scan");
        assert_eq!(wait_for_scan(&mut application).added, 2);

        let tracks = application.tracks().to_vec();
        let first = tracks
            .iter()
            .find(|track| track.title == "First")
            .expect("first scanned track");
        let second = tracks
            .iter()
            .find(|track| track.title == "Second")
            .expect("second scanned track");
        let playlist_id = application
            .create_playlist("  Morning Set  ")
            .expect("create playlist");
        application
            .add_track_to_playlist(playlist_id, first.id)
            .expect("add first track");
        application
            .add_track_to_playlist(playlist_id, second.id)
            .expect("add second track");
        application
            .move_playlist_track(playlist_id, 0, 1)
            .expect("reorder playlist");
        application
            .rename_playlist(playlist_id, "Morning Drive")
            .expect("rename playlist");
        application
            .queue_playlist(playlist_id)
            .expect("queue playlist");
        assert_eq!(
            application.playback().queue_track_ids,
            [second.id, first.id]
        );

        let mut reopened = DesktopApplication::new();
        reopened
            .begin_scan(directory.path())
            .expect("rescan library");
        wait_for_scan(&mut reopened);
        let saved = reopened
            .playlists()
            .into_iter()
            .find(|playlist| playlist.id == playlist_id)
            .expect("saved playlist");
        assert_eq!(saved.name, "Morning Drive");
        assert_eq!(saved.track_count, 2);
        let saved_tracks = reopened
            .playlist_tracks(playlist_id)
            .expect("resolve saved playlist entries");
        assert_eq!(
            saved_tracks
                .iter()
                .map(|entry| entry.track.as_ref().map(|track| track.title.as_str()))
                .collect::<Vec<_>>(),
            [Some("Second"), Some("First")]
        );

        fs::remove_file(&saved_tracks[0].path).expect("remove a saved playlist track");
        let mut rescanned = DesktopApplication::new();
        rescanned
            .begin_scan(directory.path())
            .expect("scan after a saved track was removed");
        wait_for_scan(&mut rescanned);
        let entries = rescanned
            .playlist_tracks(playlist_id)
            .expect("show playlist entries after rescan");
        assert!(entries[0].track.is_none());
        assert_eq!(
            entries[1].track.as_ref().map(|track| track.title.as_str()),
            Some("First")
        );
        assert!(
            rescanned
                .queue_playlist(playlist_id)
                .expect_err("queueing must report the missing entry")
                .contains("playlist track path is not in the library")
        );
        assert!(rescanned.playback().queue_track_ids.is_empty());
    }

    #[test]
    fn corrupt_playlist_store_is_reported_and_never_overwritten() {
        let directory = TemporaryDirectory::new();
        write_test_wav(&directory.path().join("Sample.wav"));
        let cache = directory.path().join(".aurelis_cache");
        fs::create_dir_all(&cache).expect("create test library cache");
        let playlist_path = cache.join("playlists.json");
        let corrupt_data = b"not a playlist file";
        fs::write(&playlist_path, corrupt_data).expect("write corrupt playlist data");

        let mut application = DesktopApplication::new();
        application
            .begin_scan(directory.path())
            .expect("start library scan");
        wait_for_scan(&mut application);

        assert!(!application.can_edit_playlists());
        assert!(
            application
                .playlist_storage_error()
                .is_some_and(|error| error.contains("Could not load playlists"))
        );
        assert!(
            application
                .create_playlist("Do not overwrite")
                .expect_err("corrupt saved playlists must be protected")
                .contains("protected from being overwritten")
        );
        assert_eq!(
            fs::read(&playlist_path).expect("read protected file"),
            corrupt_data
        );
    }

    #[test]
    fn listening_analytics_loads_saved_plays_and_persists_new_track_starts() {
        let directory = TemporaryDirectory::new();
        write_test_wav(&directory.path().join("Sample.wav"));
        let cache = directory.path().join(".aurelis_cache");
        fs::create_dir_all(&cache).expect("create test library cache");
        let history_path = cache.join("listening-history.json");
        let mut saved_history = ListeningHistory::default();
        saved_history
            .record_play(ListeningPlay {
                path: directory.path().join("Earlier.flac"),
                title: "Earlier track".to_owned(),
                artist: "Earlier artist".to_owned(),
                album: "Earlier album".to_owned(),
                played_at_unix_seconds: 1_700_000_000,
            })
            .unwrap();
        saved_history
            .save_to_path(&history_path)
            .expect("save existing listening history");

        let mut application = DesktopApplication::new();
        application
            .begin_scan(directory.path())
            .expect("start library scan");
        wait_for_scan(&mut application);

        assert_eq!(application.listening_analytics().total_play_starts, 1);
        assert_eq!(
            application.listening_analytics().recent_plays[0].title,
            "Earlier track"
        );

        let track_id = application.tracks()[0].id;
        application.record_track_start(track_id);
        assert_eq!(application.listening_analytics().total_play_starts, 2);
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            application.poll_listening_history_writer();
            let loaded = ListeningHistory::load_from_path(&history_path);
            if loaded.as_ref().is_ok_and(|history| history.len() == 2) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "listening history was not persisted before the deadline: {loaded:?}"
            );
            std::thread::yield_now();
        }
        assert_eq!(application.listening_history_storage_error(), None);
    }

    #[test]
    fn corrupt_listening_history_is_reported_and_never_overwritten() {
        let directory = TemporaryDirectory::new();
        write_test_wav(&directory.path().join("Sample.wav"));
        let cache = directory.path().join(".aurelis_cache");
        fs::create_dir_all(&cache).expect("create test library cache");
        let history_path = cache.join("listening-history.json");
        let corrupt_data = b"not a listening history file";
        fs::write(&history_path, corrupt_data).expect("write corrupt history data");

        let mut application = DesktopApplication::new();
        application
            .begin_scan(directory.path())
            .expect("start library scan");
        wait_for_scan(&mut application);
        let track_id = application.tracks()[0].id;
        application.record_track_start(track_id);

        assert!(
            application
                .listening_history_storage_error()
                .is_some_and(|error| error.contains("Could not load listening history"))
        );
        assert_eq!(
            fs::read(&history_path).expect("read protected history file"),
            corrupt_data
        );
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
