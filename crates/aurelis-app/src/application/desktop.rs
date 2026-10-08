use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::Duration,
};

use aurelis_core::library::{
    Artwork, ArtworkCache, ArtworkFormat, ArtworkSource, MusicLibrary, Track, TrackId,
};

use crate::player_controller::{PlaybackControlTarget, PlaybackStatus, PlayerController};

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
pub struct PlaybackView {
    pub current_track_id: Option<TrackId>,
    pub state: DesktopPlaybackState,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub queue_track_ids: Vec<TrackId>,
    pub current_queue_index: Option<usize>,
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
}

/// Application facade used by the native desktop presentation.
pub struct DesktopApplication {
    library: MusicLibrary,
    tracks: Vec<TrackSummary>,
    player: PlayerController,
    artwork_cache: Arc<Mutex<ArtworkCache>>,
    artwork_result_rx: Option<Receiver<Result<Option<Arc<Artwork>>, String>>>,
    artwork_loading_track: Option<TrackId>,
    library_root: Option<PathBuf>,
    scan_result_rx: Option<Receiver<ScanResult>>,
}

impl DesktopApplication {
    pub fn new() -> Self {
        aurelis_core::initialize();
        Self {
            library: MusicLibrary::new(),
            tracks: Vec::new(),
            player: PlayerController::new(),
            artwork_cache: Arc::new(Mutex::new(ArtworkCache::new())),
            artwork_result_rx: None,
            artwork_loading_track: None,
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
                let _ = result_tx.send(ScanResult {
                    root: worker_root,
                    library,
                    summary,
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
        self.tracks = self.library.iter().map(TrackSummary::from).collect();
        self.library_root = Some(received.root);
        self.artwork_result_rx = None;
        self.artwork_loading_track = None;
        self.artwork_cache = Arc::new(Mutex::new(ArtworkCache::new()));

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
        }
    }

    pub fn play_track(&mut self, track_id: TrackId) -> Result<(), String> {
        self.player
            .play_track(&self.library, track_id)
            .map_err(|error| error.to_string())
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

    use super::DesktopApplication;

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
}
