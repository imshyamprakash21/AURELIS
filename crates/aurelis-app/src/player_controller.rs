use std::{error::Error, fmt, io, path::Path, time::Duration};

use aurelis_core::{
    audio::{PlaybackEndReason, PlaybackEngine, SpectrumSnapshot},
    library::{MusicLibrary, Track, TrackId},
};

type AppResult<T> = Result<T, Box<dyn Error>>;

/// Application-level playback state for CLI and future UI consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaybackStatus {
    Stopped,
    Playing,
    Paused,
    Finished,
}

/// Read-only copy of the queue state at a point in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueueSnapshot {
    track_ids: Vec<TrackId>,
    current_index: Option<usize>,
}

impl QueueSnapshot {
    pub(crate) fn track_ids(&self) -> &[TrackId] {
        &self.track_ids
    }

    pub(crate) fn len(&self) -> usize {
        self.track_ids.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.track_ids.is_empty()
    }

    pub(crate) fn current_index(&self) -> Option<usize> {
        self.current_index
    }

    pub(crate) fn next_track_id(&self) -> Option<TrackId> {
        match self.current_index {
            Some(index) => index
                .checked_add(1)
                .and_then(|index| self.track_ids.get(index).copied()),
            None => self.track_ids.first().copied(),
        }
    }

    pub(crate) fn previous_track_id(&self) -> Option<TrackId> {
        self.current_index
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| self.track_ids.get(index).copied())
    }
}

/// A lightweight, read-only view of application playback at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaybackSnapshot {
    current_track_id: Option<TrackId>,
    state: PlaybackStatus,
    position: Duration,
    duration: Option<Duration>,
    queue: QueueSnapshot,
}

impl PlaybackSnapshot {
    pub(crate) fn current_track_id(&self) -> Option<TrackId> {
        self.current_track_id
    }

    pub(crate) fn state(&self) -> PlaybackStatus {
        self.state
    }

    pub(crate) fn position(&self) -> Duration {
        self.position
    }

    pub(crate) fn duration(&self) -> Option<Duration> {
        self.duration
    }

    pub(crate) fn queue(&self) -> &QueueSnapshot {
        &self.queue
    }
}

/// Playback operations the CLI needs, with a fakeable seam for app tests.
pub(crate) trait PlaybackControlTarget {
    fn play(&mut self) -> AppResult<()>;
    fn pause(&mut self);
    fn resume(&mut self);
    fn stop(&mut self);
    fn seek_by_seconds(&mut self, offset_seconds: i64) -> AppResult<()>;
    fn position(&self) -> Duration;
    fn total_duration(&self) -> Option<Duration>;
    fn poll(
        &mut self,
        library: Option<&MusicLibrary>,
    ) -> Option<Result<PlaybackEndReason, Box<dyn Error>>>;
    fn wait(&mut self) -> Result<PlaybackEndReason, Box<dyn Error>>;

    fn set_volume(&mut self, _volume: f32) {}

    fn spectrum_snapshot(&self) -> SpectrumSnapshot {
        SpectrumSnapshot::default()
    }

    fn current_track_id(&self) -> Option<TrackId> {
        None
    }

    fn playback_snapshot(&self, _library: Option<&MusicLibrary>) -> Option<PlaybackSnapshot> {
        None
    }

    fn queue_enabled(&self) -> bool {
        false
    }

    fn next_track(&mut self, _library: Option<&MusicLibrary>) -> AppResult<Option<TrackId>> {
        Err("Track navigation is only available in library playback".into())
    }

    fn previous_track(&mut self, _library: Option<&MusicLibrary>) -> AppResult<Option<TrackId>> {
        Err("Track navigation is only available in library playback".into())
    }

    fn clear_queue_entries(&mut self) -> AppResult<()> {
        Err("Queue controls are only available in library playback".into())
    }
}

impl PlaybackControlTarget for PlaybackEngine {
    fn play(&mut self) -> AppResult<()> {
        PlaybackEngine::play(self)
    }

    fn pause(&mut self) {
        PlaybackEngine::pause(self);
    }

    fn resume(&mut self) {
        PlaybackEngine::resume(self);
    }

    fn stop(&mut self) {
        PlaybackEngine::stop(self);
    }

    fn seek_by_seconds(&mut self, offset_seconds: i64) -> AppResult<()> {
        PlaybackEngine::seek_by_seconds(self, offset_seconds)
    }

    fn position(&self) -> Duration {
        PlaybackEngine::position(self)
    }

    fn total_duration(&self) -> Option<Duration> {
        PlaybackEngine::metadata(self).duration
    }

    fn poll(
        &mut self,
        _library: Option<&MusicLibrary>,
    ) -> Option<Result<PlaybackEndReason, Box<dyn Error>>> {
        PlaybackEngine::poll_end(self)
    }

    fn wait(&mut self) -> Result<PlaybackEndReason, Box<dyn Error>> {
        PlaybackEngine::wait_end(self)
    }

    fn spectrum_snapshot(&self) -> SpectrumSnapshot {
        PlaybackEngine::spectrum_snapshot(self)
    }

    fn set_volume(&mut self, volume: f32) {
        PlaybackEngine::set_volume(self, volume);
    }
}

trait PlaybackFactory {
    fn open(&self, path: &Path) -> AppResult<Box<dyn PlaybackControlTarget>>;
}

struct AudioEngineFactory;

impl PlaybackFactory for AudioEngineFactory {
    fn open(&self, path: &Path) -> AppResult<Box<dyn PlaybackControlTarget>> {
        let path = path.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the playback engine currently requires a Unicode file path",
            )
        })?;

        Ok(Box::new(PlaybackEngine::open(path)?))
    }
}

#[derive(Debug, Default)]
struct PlaybackQueue {
    entries: Vec<TrackId>,
    current_index: Option<usize>,
}

impl PlaybackQueue {
    fn next_index(&self) -> Option<usize> {
        match self.current_index {
            Some(index) => index
                .checked_add(1)
                .filter(|next| *next < self.entries.len()),
            None => (!self.entries.is_empty()).then_some(0),
        }
    }

    fn previous_index(&self) -> Option<usize> {
        self.current_index.and_then(|index| index.checked_sub(1))
    }

    fn index_of(&self, track_id: TrackId) -> Option<usize> {
        self.current_index
            .filter(|index| self.entries.get(*index) == Some(&track_id))
            .or_else(|| self.entries.iter().position(|entry| *entry == track_id))
    }

    fn upcoming_start(&self) -> usize {
        self.current_index
            .map_or(0, |index| index.saturating_add(1))
    }

    fn remove_upcoming(&mut self, index: usize) -> Option<TrackId> {
        if index < self.upcoming_start() || index >= self.entries.len() {
            return None;
        }
        Some(self.entries.remove(index))
    }

    fn move_upcoming(&mut self, from: usize, to: usize) -> bool {
        let upcoming_start = self.upcoming_start();
        if from < upcoming_start
            || to < upcoming_start
            || from >= self.entries.len()
            || to >= self.entries.len()
        {
            return false;
        }
        if from != to {
            let track_id = self.entries.remove(from);
            self.entries.insert(to, track_id);
        }
        true
    }

    fn clear_upcoming(&mut self) -> usize {
        let previous_len = self.entries.len();
        if let Some(current_index) = self.current_index {
            self.entries
                .truncate(current_index.saturating_add(1).min(previous_len));
        } else {
            self.entries.clear();
        }
        previous_len - self.entries.len()
    }

    fn select(&mut self, track_id: TrackId, existing_index: Option<usize>) {
        if let Some(index) = existing_index {
            self.current_index = Some(index);
        } else if self.entries.is_empty() {
            self.entries.push(track_id);
            self.current_index = Some(0);
        } else if let Some(current_index) = self.current_index {
            self.entries.truncate(current_index + 1);
            self.entries.push(track_id);
            self.current_index = Some(self.entries.len() - 1);
        } else {
            self.entries.insert(0, track_id);
            self.current_index = Some(0);
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.current_index = None;
    }
}

/// The application-owned playback state, kept separately from the audio
/// worker handle and derived from the library's stable TrackIds.
#[derive(Debug)]
struct PlaybackSession {
    current_track_id: Option<TrackId>,
    queue: PlaybackQueue,
    state: PlaybackStatus,
    last_position: Duration,
}

impl Default for PlaybackSession {
    fn default() -> Self {
        Self {
            current_track_id: None,
            queue: PlaybackQueue::default(),
            state: PlaybackStatus::Stopped,
            last_position: Duration::ZERO,
        }
    }
}

/// Owns the app-level current-track and queue lifecycle.
///
/// The library retains Track data. The controller stores TrackIds and opens
/// each playback session from the selected Track's path.
pub(crate) struct PlayerController {
    factory: Box<dyn PlaybackFactory>,
    engine: Option<Box<dyn PlaybackControlTarget>>,
    session: PlaybackSession,
    volume: f32,
}

impl PlayerController {
    pub(crate) fn new() -> Self {
        Self::with_factory(Box::new(AudioEngineFactory))
    }

    fn with_factory(factory: Box<dyn PlaybackFactory>) -> Self {
        Self {
            factory,
            engine: None,
            session: PlaybackSession::default(),
            volume: 1.0,
        }
    }

    pub(crate) fn current_track_id(&self) -> Option<TrackId> {
        self.session.current_track_id
    }

    /// Resolve the current ID through the library without copying its metadata.
    pub(crate) fn current_track<'a>(&self, library: &'a MusicLibrary) -> Option<&'a Track> {
        self.session.current_track_id.and_then(|id| library.get(id))
    }

    pub(crate) fn queue(&self) -> &[TrackId] {
        &self.session.queue.entries
    }

    pub(crate) fn playback_state(&self) -> PlaybackStatus {
        self.session.state
    }

    pub(crate) fn spectrum_snapshot(&self) -> SpectrumSnapshot {
        self.engine
            .as_ref()
            .map_or_else(SpectrumSnapshot::default, |engine| {
                engine.spectrum_snapshot()
            })
    }

    pub(crate) fn volume(&self) -> f32 {
        self.volume
    }

    pub(crate) fn set_volume(&mut self, volume: f32) {
        if !volume.is_finite() {
            return;
        }

        self.volume = volume.clamp(0.0, 1.0);
        if let Some(engine) = self.engine.as_mut() {
            engine.set_volume(self.volume);
        }
    }

    /// Copy the current app state for a UI or CLI renderer.
    pub(crate) fn snapshot(&self, library: &MusicLibrary) -> PlaybackSnapshot {
        let track = self.current_track(library);
        let duration = self
            .engine
            .as_ref()
            .and_then(|session| session.total_duration())
            .or_else(|| track.and_then(|track| track.metadata().duration));

        PlaybackSnapshot {
            current_track_id: self.current_track_id(),
            state: self.playback_state(),
            position: self.position(),
            duration,
            queue: self.queue_snapshot(),
        }
    }

    /// Return an owned queue view so callers cannot mutate the controller.
    pub(crate) fn queue_snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            track_ids: self.queue().to_vec(),
            current_index: self.session.queue.current_index,
        }
    }

    /// Start a library track. Selecting a track already in the queue moves the
    /// queue cursor. A new selection is inserted before a not-yet-started
    /// queue, or replaces the remaining queue after the current entry.
    pub(crate) fn play_track(
        &mut self,
        library: &MusicLibrary,
        track_id: TrackId,
    ) -> Result<(), PlayerControllerError> {
        let track = library
            .get(track_id)
            .ok_or(PlayerControllerError::TrackNotFound(track_id))?;
        let existing_index = self.session.queue.index_of(track_id);

        self.start_session(track.path(), track_id)?;
        self.session.queue.select(track_id, existing_index);
        Ok(())
    }

    pub(crate) fn enqueue(
        &mut self,
        library: &MusicLibrary,
        track_id: TrackId,
    ) -> Result<(), PlayerControllerError> {
        self.enqueue_tracks(library, [track_id])
    }

    /// Append several valid library tracks atomically, preserving input order.
    pub(crate) fn enqueue_tracks(
        &mut self,
        library: &MusicLibrary,
        track_ids: impl IntoIterator<Item = TrackId>,
    ) -> Result<(), PlayerControllerError> {
        let track_ids = track_ids.into_iter().collect::<Vec<_>>();
        if track_ids.is_empty() {
            return Ok(());
        }

        if let Some(&track_id) = track_ids.iter().find(|&&id| library.get(id).is_none()) {
            return Err(PlayerControllerError::TrackNotFound(track_id));
        }

        if self.session.queue.entries.is_empty() {
            if let Some(current) = self
                .session
                .current_track_id
                .filter(|id| library.get(*id).is_some())
            {
                self.session.queue.entries.push(current);
                self.session.queue.current_index = Some(0);
            }
        }

        self.session.queue.entries.extend(track_ids);
        Ok(())
    }

    pub(crate) fn clear_queue(&mut self) {
        self.session.queue.clear();
    }

    pub(crate) fn clear_upcoming_queue(&mut self) -> usize {
        self.session.queue.clear_upcoming()
    }

    pub(crate) fn remove_upcoming_track(&mut self, index: usize) -> Option<TrackId> {
        self.session.queue.remove_upcoming(index)
    }

    pub(crate) fn move_upcoming_track(&mut self, from: usize, to: usize) -> bool {
        self.session.queue.move_upcoming(from, to)
    }

    /// Start the next queued track, returning `None` at the end of the queue.
    pub(crate) fn next(
        &mut self,
        library: &MusicLibrary,
    ) -> Result<Option<TrackId>, PlayerControllerError> {
        let Some(index) = self.session.queue.next_index() else {
            return Ok(None);
        };
        self.start_queued_track(library, index)
    }

    /// Start the previous queued track, returning `None` at the beginning.
    pub(crate) fn previous(
        &mut self,
        library: &MusicLibrary,
    ) -> Result<Option<TrackId>, PlayerControllerError> {
        let Some(index) = self.session.queue.previous_index() else {
            return Ok(None);
        };
        self.start_queued_track(library, index)
    }

    /// Observe worker completion. Natural completion advances the queue once;
    /// a manual stop or an error never advances it.
    pub(crate) fn poll_playback(
        &mut self,
        library: &MusicLibrary,
    ) -> Option<Result<PlaybackEndReason, Box<dyn Error>>> {
        let (result, position) = {
            let session = self.engine.as_mut()?;
            (session.poll(None), session.position())
        };
        match result {
            None => None,
            Some(Err(error)) => {
                self.session.last_position = position;
                self.engine = None;
                self.session.state = PlaybackStatus::Stopped;
                Some(Err(error))
            }
            Some(Ok(PlaybackEndReason::Stopped)) => {
                self.session.last_position = position;
                self.engine = None;
                self.session.state = PlaybackStatus::Stopped;
                Some(Ok(PlaybackEndReason::Stopped))
            }
            Some(Ok(PlaybackEndReason::Completed)) => {
                self.session.last_position = position;
                self.engine = None;
                self.session.state = PlaybackStatus::Finished;

                let Some(index) = self.session.queue.next_index() else {
                    return Some(Ok(PlaybackEndReason::Completed));
                };
                match self.start_queued_track(library, index) {
                    Ok(_) => None,
                    Err(error) => {
                        self.session.state = PlaybackStatus::Finished;
                        Some(Err(Box::new(error)))
                    }
                }
            }
        }
    }

    /// Stop and join the active session without changing the queue or
    /// interpreting the stop as natural completion.
    pub(crate) fn shutdown(&mut self) -> Result<(), PlayerControllerError> {
        self.stop_current_session()?;
        self.session.state = PlaybackStatus::Stopped;
        Ok(())
    }

    fn start_queued_track(
        &mut self,
        library: &MusicLibrary,
        index: usize,
    ) -> Result<Option<TrackId>, PlayerControllerError> {
        let Some(&track_id) = self.session.queue.entries.get(index) else {
            return Ok(None);
        };

        let track = library
            .get(track_id)
            .ok_or(PlayerControllerError::TrackNotFound(track_id))?;
        self.start_session(track.path(), track_id)?;
        self.session.queue.current_index = Some(index);
        Ok(Some(track_id))
    }

    fn start_session(
        &mut self,
        path: &Path,
        track_id: TrackId,
    ) -> Result<(), PlayerControllerError> {
        self.stop_current_session()?;

        let mut session = self
            .factory
            .open(path)
            .map_err(PlayerControllerError::Playback)?;
        session.set_volume(self.volume);
        if let Err(error) = session.play() {
            self.session.state = PlaybackStatus::Stopped;
            return Err(PlayerControllerError::Playback(error));
        }

        self.engine = Some(session);
        self.session.current_track_id = Some(track_id);
        self.session.state = PlaybackStatus::Playing;
        self.session.last_position = Duration::ZERO;
        Ok(())
    }

    fn stop_current_session(&mut self) -> Result<(), PlayerControllerError> {
        self.session.state = PlaybackStatus::Stopped;
        if let Some(mut session) = self.engine.take() {
            self.session.last_position = session.position();
            session.stop();
            session.wait().map_err(PlayerControllerError::Playback)?;
        }
        Ok(())
    }
}

impl PlaybackControlTarget for PlayerController {
    fn play(&mut self) -> AppResult<()> {
        let Some(session) = self.engine.as_mut() else {
            return Err("No track is selected for playback".into());
        };
        session.play()?;
        self.session.state = PlaybackStatus::Playing;
        Ok(())
    }

    fn pause(&mut self) {
        if self.session.state == PlaybackStatus::Playing {
            if let Some(session) = self.engine.as_mut() {
                session.pause();
                self.session.state = PlaybackStatus::Paused;
            }
        }
    }

    fn resume(&mut self) {
        if self.session.state == PlaybackStatus::Paused {
            if let Some(session) = self.engine.as_mut() {
                session.resume();
                self.session.state = PlaybackStatus::Playing;
            }
        }
    }

    fn stop(&mut self) {
        if let Some(session) = self.engine.as_mut() {
            self.session.last_position = session.position();
            session.stop();
        }
        self.session.state = PlaybackStatus::Stopped;
    }

    fn seek_by_seconds(&mut self, offset_seconds: i64) -> AppResult<()> {
        let Some(session) = self.engine.as_mut() else {
            return Err("Cannot seek unless playback is active".into());
        };
        session.seek_by_seconds(offset_seconds)
    }

    fn position(&self) -> Duration {
        match self.session.state {
            PlaybackStatus::Playing | PlaybackStatus::Paused => self
                .engine
                .as_ref()
                .map_or(self.session.last_position, |session| session.position()),
            PlaybackStatus::Stopped | PlaybackStatus::Finished => self.session.last_position,
        }
    }

    fn total_duration(&self) -> Option<Duration> {
        self.engine
            .as_ref()
            .and_then(|session| session.total_duration())
    }

    fn poll(
        &mut self,
        library: Option<&MusicLibrary>,
    ) -> Option<Result<PlaybackEndReason, Box<dyn Error>>> {
        match library {
            Some(library) => self.poll_playback(library),
            None => None,
        }
    }

    fn wait(&mut self) -> Result<PlaybackEndReason, Box<dyn Error>> {
        let Some(session) = self.engine.as_mut() else {
            return Ok(match self.session.state {
                PlaybackStatus::Finished => PlaybackEndReason::Completed,
                PlaybackStatus::Stopped => PlaybackEndReason::Stopped,
                PlaybackStatus::Playing | PlaybackStatus::Paused => {
                    self.session.state = PlaybackStatus::Stopped;
                    PlaybackEndReason::Stopped
                }
            });
        };
        let result = session.wait();
        self.session.last_position = session.position();
        self.engine = None;
        self.session.state = match &result {
            Ok(PlaybackEndReason::Completed) => PlaybackStatus::Finished,
            Ok(PlaybackEndReason::Stopped) | Err(_) => PlaybackStatus::Stopped,
        };
        result
    }

    fn current_track_id(&self) -> Option<TrackId> {
        self.session.current_track_id
    }

    fn playback_snapshot(&self, library: Option<&MusicLibrary>) -> Option<PlaybackSnapshot> {
        library.map(|library| PlayerController::snapshot(self, library))
    }

    fn queue_enabled(&self) -> bool {
        true
    }

    fn next_track(&mut self, library: Option<&MusicLibrary>) -> AppResult<Option<TrackId>> {
        let library = library.ok_or("The music library is not available")?;
        self.next(library)
            .map_err(|error| Box::new(error) as Box<dyn Error>)
    }

    fn previous_track(&mut self, library: Option<&MusicLibrary>) -> AppResult<Option<TrackId>> {
        let library = library.ok_or("The music library is not available")?;
        self.previous(library)
            .map_err(|error| Box::new(error) as Box<dyn Error>)
    }

    fn clear_queue_entries(&mut self) -> AppResult<()> {
        self.clear_queue();
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) enum PlayerControllerError {
    TrackNotFound(TrackId),
    Playback(Box<dyn Error>),
}

impl fmt::Display for PlayerControllerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TrackNotFound(track_id) => write!(formatter, "Track not found: {track_id:?}"),
            Self::Playback(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for PlayerControllerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::TrackNotFound(_) => None,
            Self::Playback(error) => Some(error.as_ref()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aurelis_core::audio::AudioMetadata;
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    #[derive(Debug)]
    struct FakeSessionState {
        state: PlaybackStatus,
        end_reason: Option<PlaybackEndReason>,
        position: Duration,
        volume: f32,
        stop_calls: usize,
        wait_calls: usize,
        seek_offsets: Vec<i64>,
    }

    #[derive(Clone, Default)]
    struct FakeFactory {
        opened_paths: Arc<Mutex<Vec<PathBuf>>>,
        sessions: Arc<Mutex<Vec<Arc<Mutex<FakeSessionState>>>>>,
    }

    impl FakeFactory {
        fn sessions(&self) -> Vec<Arc<Mutex<FakeSessionState>>> {
            self.sessions.lock().unwrap().clone()
        }

        fn opened_paths(&self) -> Vec<PathBuf> {
            self.opened_paths.lock().unwrap().clone()
        }

        fn set_position(&self, session_index: usize, position: Duration) {
            self.sessions()[session_index].lock().unwrap().position = position;
        }
    }

    impl PlaybackFactory for FakeFactory {
        fn open(&self, path: &Path) -> AppResult<Box<dyn PlaybackControlTarget>> {
            self.opened_paths.lock().unwrap().push(path.to_path_buf());
            let state = Arc::new(Mutex::new(FakeSessionState {
                state: PlaybackStatus::Stopped,
                end_reason: None,
                position: Duration::ZERO,
                volume: 1.0,
                stop_calls: 0,
                wait_calls: 0,
                seek_offsets: Vec::new(),
            }));
            self.sessions.lock().unwrap().push(Arc::clone(&state));
            Ok(Box::new(FakeSession { state }))
        }
    }

    struct FakeSession {
        state: Arc<Mutex<FakeSessionState>>,
    }

    impl PlaybackControlTarget for FakeSession {
        fn set_volume(&mut self, volume: f32) {
            self.state.lock().unwrap().volume = volume;
        }

        fn play(&mut self) -> AppResult<()> {
            self.state.lock().unwrap().state = PlaybackStatus::Playing;
            Ok(())
        }

        fn pause(&mut self) {
            let mut state = self.state.lock().unwrap();
            if state.state == PlaybackStatus::Playing {
                state.state = PlaybackStatus::Paused;
            }
        }

        fn resume(&mut self) {
            let mut state = self.state.lock().unwrap();
            if state.state == PlaybackStatus::Paused {
                state.state = PlaybackStatus::Playing;
            }
        }

        fn stop(&mut self) {
            let mut state = self.state.lock().unwrap();
            state.stop_calls += 1;
            state.state = PlaybackStatus::Stopped;
            state.end_reason = Some(PlaybackEndReason::Stopped);
        }

        fn seek_by_seconds(&mut self, offset_seconds: i64) -> AppResult<()> {
            let mut state = self.state.lock().unwrap();
            if state.state == PlaybackStatus::Stopped {
                return Err("playback is stopped".into());
            }
            state.seek_offsets.push(offset_seconds);
            Ok(())
        }

        fn position(&self) -> Duration {
            self.state.lock().unwrap().position
        }

        fn total_duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(60))
        }

        fn poll(
            &mut self,
            _library: Option<&MusicLibrary>,
        ) -> Option<Result<PlaybackEndReason, Box<dyn Error>>> {
            let mut state = self.state.lock().unwrap();
            let reason = state.end_reason.take()?;
            state.state = PlaybackStatus::Stopped;
            Some(Ok(reason))
        }

        fn wait(&mut self) -> Result<PlaybackEndReason, Box<dyn Error>> {
            let mut state = self.state.lock().unwrap();
            state.wait_calls += 1;
            state.state = PlaybackStatus::Stopped;
            Ok(state
                .end_reason
                .take()
                .unwrap_or(PlaybackEndReason::Stopped))
        }
    }

    fn test_library() -> (MusicLibrary, TrackId, TrackId, TrackId) {
        let mut library = MusicLibrary::new();
        let metadata = AudioMetadata {
            duration: Some(Duration::from_secs(60)),
            ..AudioMetadata::default()
        };
        let first = library.add_track("music/a.flac", metadata.clone());
        let second = library.add_track("music/b.wav", metadata.clone());
        let third = library.add_track("music/c.mp3", metadata);
        (library, first, second, third)
    }

    fn controller(factory: &FakeFactory) -> PlayerController {
        PlayerController::with_factory(Box::new(factory.clone()))
    }

    #[test]
    fn volume_is_clamped_and_applied_to_current_and_future_sessions() {
        let (library, track_id, _, _) = test_library();
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);

        controller.set_volume(0.35);
        controller
            .play_track(&library, track_id)
            .expect("play track");
        let session = factory.sessions()[0].clone();
        assert_eq!(session.lock().unwrap().volume, 0.35);

        controller.set_volume(1.5);
        assert_eq!(session.lock().unwrap().volume, 1.0);
        controller.set_volume(-0.5);
        assert_eq!(session.lock().unwrap().volume, 0.0);
        controller.set_volume(f32::NAN);
        assert_eq!(controller.volume(), 0.0);
        assert_eq!(session.lock().unwrap().volume, 0.0);
    }

    fn mark_completed(factory: &FakeFactory, session_index: usize) {
        let sessions = factory.sessions();
        let mut state = sessions[session_index].lock().unwrap();
        state.state = PlaybackStatus::Stopped;
        state.end_reason = Some(PlaybackEndReason::Completed);
    }

    #[test]
    fn initial_controller_state_is_empty_and_stopped() {
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);
        let library = MusicLibrary::new();

        assert_eq!(controller.current_track_id(), None);
        assert!(controller.queue().is_empty());
        assert_eq!(controller.playback_state(), PlaybackStatus::Stopped);
        let snapshot = controller.snapshot(&library);
        assert_eq!(snapshot.current_track_id(), None);
        assert_eq!(snapshot.state(), PlaybackStatus::Stopped);
        assert_eq!(snapshot.position(), Duration::ZERO);
        assert_eq!(snapshot.duration(), None);
        assert!(snapshot.queue().is_empty());
        assert_eq!(snapshot.queue().len(), 0);
        assert_eq!(snapshot.queue().current_index(), None);
        assert_eq!(snapshot.queue().next_track_id(), None);
        assert_eq!(snapshot.queue().previous_track_id(), None);
        assert_eq!(controller.next(&test_library().0).unwrap(), None);
        assert_eq!(controller.previous(&test_library().0).unwrap(), None);
    }

    #[test]
    fn snapshot_resolves_track_and_reports_engine_position_and_duration() {
        let (library, first, _, _) = test_library();
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);
        controller.play_track(&library, first).unwrap();
        factory.set_position(0, Duration::from_secs(12));

        let snapshot = controller.snapshot(&library);
        let track = controller.current_track(&library).unwrap();

        assert_eq!(snapshot.current_track_id(), Some(first));
        assert_eq!(snapshot.state(), PlaybackStatus::Playing);
        assert_eq!(snapshot.position(), Duration::from_secs(12));
        assert_eq!(snapshot.duration(), Some(Duration::from_secs(60)));
        assert_eq!(track.id(), first);
        assert_eq!(track.display_title(), "a");
    }

    #[test]
    fn current_track_is_none_when_its_id_is_missing_from_the_library() {
        let (library, first, _, _) = test_library();
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);
        controller.play_track(&library, first).unwrap();

        let different_library = MusicLibrary::new();
        assert!(controller.current_track(&different_library).is_none());
        let snapshot = controller.snapshot(&different_library);
        assert_eq!(snapshot.current_track_id(), Some(first));
        assert_eq!(snapshot.state(), PlaybackStatus::Playing);
    }

    #[test]
    fn queue_snapshot_has_order_cursor_neighbors_and_no_mutable_controller_access() {
        let (library, first, second, third) = test_library();
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);
        controller.play_track(&library, first).unwrap();
        controller
            .enqueue_tracks(&library, [second, third])
            .unwrap();

        let initial = controller.queue_snapshot();
        assert_eq!(initial.track_ids(), &[first, second, third]);
        assert_eq!(initial.len(), 3);
        assert_eq!(initial.current_index(), Some(0));
        assert_eq!(initial.next_track_id(), Some(second));
        assert_eq!(initial.previous_track_id(), None);

        let mut caller_copy = initial.track_ids().to_vec();
        caller_copy.clear();
        assert_eq!(controller.queue(), &[first, second, third]);

        controller.next(&library).unwrap();
        let middle = controller.snapshot(&library).queue().clone();
        assert_eq!(middle.current_index(), Some(1));
        assert_eq!(middle.previous_track_id(), Some(first));
        assert_eq!(middle.next_track_id(), Some(third));
    }

    #[test]
    fn queue_edits_only_affect_upcoming_entries_and_keep_the_active_cursor() {
        let (_, first, second, third) = test_library();
        let mut queue = PlaybackQueue {
            entries: vec![first, second, third],
            current_index: Some(0),
        };

        assert!(queue.move_upcoming(1, 2));
        assert_eq!(queue.entries, [first, third, second]);
        assert_eq!(queue.current_index, Some(0));
        assert_eq!(queue.next_index(), Some(1));
        assert!(!queue.move_upcoming(0, 1));
        assert_eq!(queue.remove_upcoming(0), None);
        assert_eq!(queue.remove_upcoming(2), Some(second));
        assert_eq!(queue.entries, [first, third]);
        assert_eq!(queue.clear_upcoming(), 1);
        assert_eq!(queue.entries, [first]);
        assert_eq!(queue.current_index, Some(0));
    }

    #[test]
    fn queue_edits_work_before_playback_starts() {
        let (_, first, second, third) = test_library();
        let mut queue = PlaybackQueue {
            entries: vec![first, second, third],
            current_index: None,
        };

        assert!(queue.move_upcoming(0, 2));
        assert_eq!(queue.entries, [second, third, first]);
        assert_eq!(queue.remove_upcoming(1), Some(third));
        assert_eq!(queue.clear_upcoming(), 2);
        assert!(queue.entries.is_empty());
        assert_eq!(queue.current_index, None);
    }

    #[test]
    fn enqueue_tracks_validates_all_ids_before_changing_the_queue() {
        let (library, first, second, _) = test_library();
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);
        controller.play_track(&library, first).unwrap();

        let mut other_library = MusicLibrary::new();
        for index in 0..3 {
            other_library.add_track(format!("other/{index}.flac"), AudioMetadata::default());
        }
        let invalid = other_library.add_track("other/invalid.flac", AudioMetadata::default());
        assert!(
            controller
                .enqueue_tracks(&library, [second, invalid])
                .is_err()
        );
        assert_eq!(controller.queue(), &[first]);
    }

    #[test]
    fn snapshots_reflect_pause_resume_and_manual_stop_states() {
        let (library, first, _, _) = test_library();
        let factory = FakeFactory::default();
        let mut player = controller(&factory);
        player.play_track(&library, first).unwrap();

        player.pause();
        assert_eq!(player.snapshot(&library).state(), PlaybackStatus::Paused);
        player.resume();
        assert_eq!(player.snapshot(&library).state(), PlaybackStatus::Playing);
        factory.set_position(0, Duration::from_secs(12));
        player.stop();
        let stopped = player.snapshot(&library);
        assert_eq!(stopped.state(), PlaybackStatus::Stopped);
        assert_eq!(stopped.position(), Duration::from_secs(12));
        player.wait().unwrap();
        assert_eq!(player.snapshot(&library).state(), PlaybackStatus::Stopped);
    }

    #[test]
    fn seek_delegates_to_the_active_session_and_rejects_an_empty_session() {
        let (library, first, _, _) = test_library();
        let factory = FakeFactory::default();
        let mut player = controller(&factory);

        assert!(player.seek_by_seconds(10).is_err());
        player.play_track(&library, first).unwrap();
        player.seek_by_seconds(15).unwrap();
        player.pause();
        player.seek_by_seconds(-5).unwrap();

        let sessions = factory.sessions();
        assert_eq!(sessions[0].lock().unwrap().seek_offsets, [15, -5]);
        assert_eq!(player.playback_state(), PlaybackStatus::Paused);
    }

    #[test]
    fn playing_a_valid_id_tracks_it_and_opens_its_path() {
        let (library, first, _, _) = test_library();
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);

        controller.play_track(&library, first).unwrap();

        assert_eq!(controller.current_track_id(), Some(first));
        assert_eq!(controller.queue(), &[first]);
        assert_eq!(controller.playback_state(), PlaybackStatus::Playing);
        assert_eq!(factory.opened_paths(), vec![PathBuf::from("music/a.flac")]);
    }

    #[test]
    fn invalid_track_id_is_rejected_without_opening_a_session() {
        let (library, _, second, _) = test_library();
        let mut other_library = MusicLibrary::new();
        for index in 0..3 {
            other_library.add_track(format!("elsewhere/{index}.flac"), AudioMetadata::default());
        }
        let invalid_id =
            other_library.add_track("elsewhere/invalid.flac", AudioMetadata::default());
        assert_ne!(invalid_id, second);

        let factory = FakeFactory::default();
        let mut controller = controller(&factory);
        let error = controller.play_track(&library, invalid_id).unwrap_err();

        assert!(error.to_string().contains("Track not found"));
        assert!(factory.opened_paths().is_empty());
        assert_eq!(controller.current_track_id(), None);
    }

    #[test]
    fn replacing_a_track_stops_and_joins_the_old_session_before_starting_new_one() {
        let (library, first, second, _) = test_library();
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);

        controller.play_track(&library, first).unwrap();
        controller.play_track(&library, second).unwrap();

        let sessions = factory.sessions();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].lock().unwrap().stop_calls, 1);
        assert_eq!(sessions[0].lock().unwrap().wait_calls, 1);
        assert_eq!(sessions[0].lock().unwrap().state, PlaybackStatus::Stopped);
        assert_eq!(sessions[1].lock().unwrap().state, PlaybackStatus::Playing);
        assert_eq!(controller.current_track_id(), Some(second));
        assert_eq!(controller.queue(), &[first, second]);
    }

    #[test]
    fn enqueue_preserves_order_and_rejects_ids_outside_the_library() {
        let (library, first, second, third) = test_library();
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);
        controller.play_track(&library, first).unwrap();

        controller.enqueue(&library, second).unwrap();
        controller.enqueue(&library, third).unwrap();

        assert_eq!(controller.queue(), &[first, second, third]);
        let mut other_library = MusicLibrary::new();
        for index in 0..3 {
            other_library.add_track(format!("other/{index}.flac"), AudioMetadata::default());
        }
        let invalid_id = other_library.add_track("other/invalid.flac", AudioMetadata::default());
        assert!(controller.enqueue(&library, invalid_id).is_err());
        assert_eq!(controller.queue(), &[first, second, third]);
    }

    #[test]
    fn selecting_initial_track_preserves_tracks_enqueued_before_playback() {
        let (library, first, second, third) = test_library();
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);

        controller.enqueue(&library, second).unwrap();
        controller.enqueue(&library, third).unwrap();
        let queued = controller.queue_snapshot();
        assert_eq!(queued.current_index(), None);
        assert_eq!(queued.next_track_id(), Some(second));
        assert_eq!(queued.previous_track_id(), None);
        controller.play_track(&library, first).unwrap();

        assert_eq!(controller.queue(), &[first, second, third]);
        assert_eq!(controller.next(&library).unwrap(), Some(second));
    }

    #[test]
    fn selecting_a_new_track_replaces_only_the_unplayed_queue_tail() {
        let (mut library, first, second, third) = test_library();
        let fourth = library.add_track("music/d.flac", AudioMetadata::default());
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);

        controller.play_track(&library, first).unwrap();
        controller
            .enqueue_tracks(&library, [second, third])
            .unwrap();
        controller.play_track(&library, fourth).unwrap();

        assert_eq!(controller.queue(), &[first, fourth]);
        assert_eq!(controller.queue_snapshot().current_index(), Some(1));
        assert_eq!(controller.current_track_id(), Some(fourth));
    }

    #[test]
    fn next_and_previous_navigate_in_queue_order() {
        let (library, first, second, third) = test_library();
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);
        controller.play_track(&library, first).unwrap();
        controller.enqueue(&library, second).unwrap();
        controller.enqueue(&library, third).unwrap();

        assert_eq!(controller.next(&library).unwrap(), Some(second));
        assert_eq!(controller.current_track_id(), Some(second));
        assert_eq!(controller.previous(&library).unwrap(), Some(first));
        assert_eq!(controller.current_track_id(), Some(first));
        assert_eq!(factory.sessions().len(), 3);
    }

    #[test]
    fn next_at_queue_end_and_previous_at_queue_start_do_not_move() {
        let (library, first, second, _) = test_library();
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);
        controller.play_track(&library, first).unwrap();
        controller.enqueue(&library, second).unwrap();

        assert_eq!(controller.previous(&library).unwrap(), None);
        assert_eq!(controller.next(&library).unwrap(), Some(second));
        assert_eq!(controller.next(&library).unwrap(), None);
        assert_eq!(controller.current_track_id(), Some(second));
        assert_eq!(controller.playback_state(), PlaybackStatus::Playing);
    }

    #[test]
    fn clearing_queue_keeps_current_playback_but_removes_navigation_entries() {
        let (library, first, second, _) = test_library();
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);
        controller.play_track(&library, first).unwrap();
        controller.enqueue(&library, second).unwrap();

        controller.clear_queue();

        assert!(controller.queue().is_empty());
        assert_eq!(controller.current_track_id(), Some(first));
        assert_eq!(controller.next(&library).unwrap(), None);
        assert_eq!(controller.playback_state(), PlaybackStatus::Playing);
        assert!(controller.snapshot(&library).queue().is_empty());
    }

    #[test]
    fn natural_completion_starts_the_next_track_automatically() {
        let (library, first, second, _) = test_library();
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);
        controller.play_track(&library, first).unwrap();
        controller.enqueue(&library, second).unwrap();
        mark_completed(&factory, 0);

        assert!(controller.poll_playback(&library).is_none());

        assert_eq!(factory.sessions().len(), 2);
        assert_eq!(controller.current_track_id(), Some(second));
        assert_eq!(controller.playback_state(), PlaybackStatus::Playing);
        assert_eq!(
            factory.sessions()[0].lock().unwrap().state,
            PlaybackStatus::Stopped
        );
        assert_eq!(
            factory.sessions()[1].lock().unwrap().state,
            PlaybackStatus::Playing
        );
        let snapshot = controller.snapshot(&library);
        assert_eq!(snapshot.current_track_id(), Some(second));
        assert_eq!(snapshot.state(), PlaybackStatus::Playing);
        assert_eq!(snapshot.position(), Duration::ZERO);
        assert_eq!(snapshot.queue().current_index(), Some(1));
    }

    #[test]
    fn final_natural_completion_leaves_current_track_finished_and_is_consumed_once() {
        let (library, first, _, _) = test_library();
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);
        controller.play_track(&library, first).unwrap();
        factory.set_position(0, Duration::from_secs(60));
        mark_completed(&factory, 0);

        assert!(matches!(
            controller.poll_playback(&library),
            Some(Ok(PlaybackEndReason::Completed))
        ));
        assert_eq!(controller.current_track_id(), Some(first));
        assert_eq!(controller.playback_state(), PlaybackStatus::Finished);
        let snapshot = controller.snapshot(&library);
        assert_eq!(snapshot.state(), PlaybackStatus::Finished);
        assert_eq!(snapshot.position(), Duration::from_secs(60));
        assert_eq!(controller.wait().unwrap(), PlaybackEndReason::Completed);
        assert_eq!(controller.playback_state(), PlaybackStatus::Finished);
        assert!(controller.poll_playback(&library).is_none());
        assert_eq!(factory.sessions().len(), 1);
    }

    #[test]
    fn manual_stop_does_not_advance_the_queue() {
        let (library, first, second, _) = test_library();
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);
        controller.play_track(&library, first).unwrap();
        controller.enqueue(&library, second).unwrap();

        controller.stop();
        assert!(matches!(
            controller.poll_playback(&library),
            Some(Ok(PlaybackEndReason::Stopped))
        ));

        assert_eq!(controller.current_track_id(), Some(first));
        assert_eq!(controller.playback_state(), PlaybackStatus::Stopped);
        assert_eq!(factory.sessions().len(), 1);
    }

    #[test]
    fn pause_is_not_treated_as_completion() {
        let (library, first, second, _) = test_library();
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);
        controller.play_track(&library, first).unwrap();
        controller.enqueue(&library, second).unwrap();

        controller.pause();

        assert!(controller.poll_playback(&library).is_none());
        assert_eq!(controller.current_track_id(), Some(first));
        assert_eq!(controller.playback_state(), PlaybackStatus::Paused);
        assert_eq!(factory.sessions().len(), 1);
    }

    #[test]
    fn shutdown_does_not_advance_the_queue() {
        let (library, first, second, _) = test_library();
        let factory = FakeFactory::default();
        let mut controller = controller(&factory);
        controller.play_track(&library, first).unwrap();
        controller.enqueue(&library, second).unwrap();

        controller.shutdown().unwrap();

        assert_eq!(controller.current_track_id(), Some(first));
        assert_eq!(controller.playback_state(), PlaybackStatus::Stopped);
        assert_eq!(factory.sessions().len(), 1);
        assert_eq!(factory.sessions()[0].lock().unwrap().wait_calls, 1);
    }
}
