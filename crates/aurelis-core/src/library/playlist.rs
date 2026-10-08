use std::{
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::{MusicLibrary, Track, TrackId};

/// An opaque ID assigned by a [`PlaylistCollection`].
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct PlaylistId(u64);

/// A saved reference to a track, stored by its library path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlaylistTrack {
    path: PathBuf,
}

impl PlaylistTrack {
    /// The path used to identify this track in a [`MusicLibrary`].
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// An ordered, named list of saved track references.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Playlist {
    id: PlaylistId,
    name: String,
    tracks: Vec<PlaylistTrack>,
}

impl Playlist {
    /// The ID assigned by the containing collection.
    pub const fn id(&self) -> PlaylistId {
        self.id
    }

    /// The playlist name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The ordered saved track references.
    pub fn tracks(&self) -> &[PlaylistTrack] {
        &self.tracks
    }

    /// The number of saved tracks.
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    /// Whether the playlist has no tracks.
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }
}

/// A local collection of persistent playlists, separate from playback queues.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlaylistCollection {
    playlists: Vec<Playlist>,
    next_id: u64,
}

impl PlaylistCollection {
    /// Create an empty collection. Playlist IDs start at 1.
    pub const fn new() -> Self {
        Self {
            playlists: Vec::new(),
            next_id: 1,
        }
    }

    /// The number of playlists in the collection.
    pub fn len(&self) -> usize {
        self.playlists.len()
    }

    /// Whether the collection contains no playlists.
    pub fn is_empty(&self) -> bool {
        self.playlists.is_empty()
    }

    /// Iterate over playlists in insertion order.
    pub fn iter(&self) -> std::slice::Iter<'_, Playlist> {
        self.playlists.iter()
    }

    /// Find a playlist by its collection-assigned ID.
    pub fn get(&self, id: PlaylistId) -> Option<&Playlist> {
        self.playlists.iter().find(|playlist| playlist.id == id)
    }

    /// Create a playlist with a non-empty name.
    pub fn create_playlist(&mut self, name: impl AsRef<str>) -> Result<PlaylistId, PlaylistError> {
        let name = valid_name(name.as_ref())?;
        let next_id = self
            .next_id
            .checked_add(1)
            .ok_or(PlaylistError::IdExhausted)?;
        let id = PlaylistId(self.next_id);
        self.next_id = next_id;
        self.playlists.push(Playlist {
            id,
            name,
            tracks: Vec::new(),
        });
        Ok(id)
    }

    /// Rename a playlist, trimming surrounding whitespace from the new name.
    pub fn rename_playlist(
        &mut self,
        id: PlaylistId,
        name: impl AsRef<str>,
    ) -> Result<(), PlaylistError> {
        let name = valid_name(name.as_ref())?;
        self.playlist_mut(id)?.name = name;
        Ok(())
    }

    /// Delete a playlist and return its former value.
    pub fn delete_playlist(&mut self, id: PlaylistId) -> Result<Playlist, PlaylistError> {
        let index = self
            .playlists
            .iter()
            .position(|playlist| playlist.id == id)
            .ok_or(PlaylistError::PlaylistNotFound(id))?;
        Ok(self.playlists.remove(index))
    }

    /// Append a track reference to a playlist.
    ///
    /// Repeated tracks are allowed and retain their distinct positions.
    pub fn add_track(&mut self, id: PlaylistId, track: &Track) -> Result<(), PlaylistError> {
        self.playlist_mut(id)?.tracks.push(PlaylistTrack {
            path: track.path().to_path_buf(),
        });
        Ok(())
    }

    /// Remove and return the track reference at `index`.
    pub fn remove_track(
        &mut self,
        id: PlaylistId,
        index: usize,
    ) -> Result<PlaylistTrack, PlaylistError> {
        let playlist = self.playlist_mut(id)?;
        if index >= playlist.tracks.len() {
            return Err(PlaylistError::TrackIndexOutOfBounds {
                index,
                len: playlist.tracks.len(),
            });
        }
        Ok(playlist.tracks.remove(index))
    }

    /// Move a track from one position to another without changing other order.
    pub fn move_track(
        &mut self,
        id: PlaylistId,
        from: usize,
        to: usize,
    ) -> Result<(), PlaylistError> {
        let playlist = self.playlist_mut(id)?;
        let len = playlist.tracks.len();
        if from >= len {
            return Err(PlaylistError::TrackIndexOutOfBounds { index: from, len });
        }
        if to >= len {
            return Err(PlaylistError::TrackIndexOutOfBounds { index: to, len });
        }
        let track = playlist.tracks.remove(from);
        playlist.tracks.insert(to, track);
        Ok(())
    }

    /// Resolve a saved playlist to the current library's track IDs in order.
    ///
    /// Returns an error when a referenced path is not present, rather than
    /// silently dropping that playlist entry. Duplicate entries remain
    /// duplicated in the returned IDs.
    pub fn resolve_track_ids(
        &self,
        id: PlaylistId,
        library: &MusicLibrary,
    ) -> Result<Vec<TrackId>, PlaylistError> {
        let playlist = self.get(id).ok_or(PlaylistError::PlaylistNotFound(id))?;
        playlist
            .tracks
            .iter()
            .map(|entry| {
                library
                    .get_by_path(&entry.path)
                    .map(Track::id)
                    .ok_or_else(|| PlaylistError::MissingTrack(entry.path.clone()))
            })
            .collect()
    }

    /// Save this collection as a JSON file at `path`.
    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<(), PlaylistError> {
        let path = path.as_ref();
        let contents = serde_json::to_vec_pretty(self).map_err(PlaylistError::Serialization)?;
        fs::write(path, contents).map_err(|source| PlaylistError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Load and validate a JSON playlist collection from `path`.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, PlaylistError> {
        let path = path.as_ref();
        let contents = fs::read(path).map_err(|source| PlaylistError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let collection: Self =
            serde_json::from_slice(&contents).map_err(PlaylistError::Serialization)?;
        collection.validate()?;
        Ok(collection)
    }

    fn playlist_mut(&mut self, id: PlaylistId) -> Result<&mut Playlist, PlaylistError> {
        self.playlists
            .iter_mut()
            .find(|playlist| playlist.id == id)
            .ok_or(PlaylistError::PlaylistNotFound(id))
    }

    fn validate(&self) -> Result<(), PlaylistError> {
        let mut ids = std::collections::HashSet::with_capacity(self.playlists.len());
        let mut max_id = 0;
        for playlist in &self.playlists {
            if playlist.id.0 == 0 || !ids.insert(playlist.id) {
                return Err(PlaylistError::InvalidData(
                    "playlist IDs must be unique and nonzero",
                ));
            }
            if playlist.name.trim().is_empty() {
                return Err(PlaylistError::InvalidData(
                    "playlist names must not be empty",
                ));
            }
            max_id = max_id.max(playlist.id.0);
        }
        if self.next_id == 0 || self.next_id <= max_id {
            return Err(PlaylistError::InvalidData(
                "next playlist ID must exceed existing IDs",
            ));
        }
        Ok(())
    }
}

impl Default for PlaylistCollection {
    fn default() -> Self {
        Self::new()
    }
}

fn valid_name(name: &str) -> Result<String, PlaylistError> {
    let name = name.trim();
    if name.is_empty() {
        Err(PlaylistError::InvalidName)
    } else {
        Ok(name.to_owned())
    }
}

/// An error encountered while editing, resolving, or persisting playlists.
#[derive(Debug)]
pub enum PlaylistError {
    InvalidName,
    PlaylistNotFound(PlaylistId),
    TrackIndexOutOfBounds { index: usize, len: usize },
    MissingTrack(PathBuf),
    IdExhausted,
    InvalidData(&'static str),
    Io { path: PathBuf, source: io::Error },
    Serialization(serde_json::Error),
}

impl fmt::Display for PlaylistError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName => formatter.write_str("playlist name must not be empty"),
            Self::PlaylistNotFound(id) => write!(formatter, "playlist {id:?} was not found"),
            Self::TrackIndexOutOfBounds { index, len } => {
                write!(
                    formatter,
                    "track index {index} is outside playlist length {len}"
                )
            }
            Self::MissingTrack(path) => {
                write!(
                    formatter,
                    "playlist track path is not in the library: {path:?}"
                )
            }
            Self::IdExhausted => formatter.write_str("playlist ID space is exhausted"),
            Self::InvalidData(message) => write!(formatter, "invalid playlist data: {message}"),
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "failed to access playlist file {path:?}: {source}"
                )
            }
            Self::Serialization(source) => {
                write!(
                    formatter,
                    "failed to encode or decode playlist data: {source}"
                )
            }
        }
    }
}

impl Error for PlaylistError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Serialization(source) => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioMetadata;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "aurelis-playlists-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create temporary playlist directory");
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

    fn test_library() -> (MusicLibrary, TrackId, TrackId, TrackId) {
        let mut library = MusicLibrary::new();
        let first = library.add_track("Music/first.flac", AudioMetadata::default());
        let second = library.add_track("Music/second.flac", AudioMetadata::default());
        let third = library.add_track("Music/third.flac", AudioMetadata::default());
        (library, first, second, third)
    }

    #[test]
    fn creates_renames_and_deletes_playlists() {
        let mut collection = PlaylistCollection::new();
        let id = collection.create_playlist("  Road trip  ").unwrap();

        assert_eq!(collection.get(id).unwrap().name(), "Road trip");
        collection.rename_playlist(id, "Late night").unwrap();
        assert_eq!(collection.get(id).unwrap().name(), "Late night");
        assert!(collection.create_playlist("  ").is_err());
        assert!(collection.rename_playlist(id, "\n").is_err());

        let deleted = collection.delete_playlist(id).unwrap();
        assert_eq!(deleted.name(), "Late night");
        assert!(collection.get(id).is_none());
        assert!(collection.delete_playlist(id).is_err());
    }

    #[test]
    fn adds_removes_and_reorders_track_references() {
        let (library, first, second, third) = test_library();
        let mut collection = PlaylistCollection::new();
        let playlist_id = collection.create_playlist("Favorites").unwrap();
        for id in [first, second, third] {
            collection
                .add_track(playlist_id, library.get(id).unwrap())
                .unwrap();
        }

        collection.move_track(playlist_id, 2, 0).unwrap();
        let removed = collection.remove_track(playlist_id, 1).unwrap();
        let playlist = collection.get(playlist_id).unwrap();
        assert_eq!(removed.path(), Path::new("Music/first.flac"));
        assert_eq!(
            playlist
                .tracks()
                .iter()
                .map(PlaylistTrack::path)
                .collect::<Vec<_>>(),
            [
                Path::new("Music/third.flac"),
                Path::new("Music/second.flac")
            ]
        );
        assert!(collection.move_track(playlist_id, 0, 2).is_err());
        assert!(collection.remove_track(playlist_id, 2).is_err());
    }

    #[test]
    fn resolves_playlist_paths_to_current_library_ids_in_order() {
        let (library, first, second, _) = test_library();
        let mut collection = PlaylistCollection::new();
        let playlist_id = collection.create_playlist("Favorites").unwrap();
        collection
            .add_track(playlist_id, library.get(second).unwrap())
            .unwrap();
        collection
            .add_track(playlist_id, library.get(first).unwrap())
            .unwrap();
        collection
            .add_track(playlist_id, library.get(second).unwrap())
            .unwrap();

        assert_eq!(
            collection.resolve_track_ids(playlist_id, &library).unwrap(),
            [second, first, second]
        );
        assert!(matches!(
            collection.resolve_track_ids(playlist_id, &MusicLibrary::new()),
            Err(PlaylistError::MissingTrack(_))
        ));
    }

    #[test]
    fn saves_and_loads_collection_with_playlist_order_and_ids() {
        let directory = TestDirectory::new();
        let path = directory.path().join("playlists.json");
        let (library, first, second, _) = test_library();
        let mut collection = PlaylistCollection::new();
        let favorites = collection.create_playlist("Favorites").unwrap();
        let later = collection.create_playlist("Later").unwrap();
        collection
            .add_track(favorites, library.get(second).unwrap())
            .unwrap();
        collection
            .add_track(favorites, library.get(first).unwrap())
            .unwrap();

        collection.save_to_path(&path).unwrap();
        let loaded = PlaylistCollection::load_from_path(&path).unwrap();

        assert_eq!(loaded, collection);
        assert_eq!(loaded.get(later).unwrap().name(), "Later");
        let next = {
            let mut loaded = loaded;
            loaded.create_playlist("Next").unwrap()
        };
        assert_ne!(next, favorites);
        assert_ne!(next, later);
    }

    #[test]
    fn rejects_corrupt_playlist_files() {
        let directory = TestDirectory::new();
        let path = directory.path().join("playlists.json");
        fs::write(&path, b"not json").expect("write malformed playlist data");

        assert!(matches!(
            PlaylistCollection::load_from_path(&path),
            Err(PlaylistError::Serialization(_))
        ));
    }
}
