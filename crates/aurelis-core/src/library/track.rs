use std::{
    borrow::Cow,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use crate::audio::AudioMetadata;

/// An opaque ID assigned by a [`super::MusicLibrary`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TrackId(u64);

impl TrackId {
    pub(super) const fn new(value: u64) -> Self {
        Self(value)
    }

    pub(super) const fn value(self) -> u64 {
        self.0
    }
}

/// A catalog entry for one audio file.
///
/// Creating a track does not open or inspect its path. `MusicLibrary` assigns
/// its ID when the track is added.
#[derive(Clone, Debug)]
pub struct Track {
    id: TrackId,
    path: PathBuf,
    metadata: AudioMetadata,
}

impl Track {
    pub(super) fn new(id: TrackId, path: PathBuf, metadata: AudioMetadata) -> Self {
        Self { id, path, metadata }
    }

    /// The ID assigned by the containing library.
    pub const fn id(&self) -> TrackId {
        self.id
    }

    /// The path exactly as stored by the library.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The metadata extracted for this track.
    pub fn metadata(&self) -> &AudioMetadata {
        &self.metadata
    }

    /// Return the title tag, or the file stem when the tag is missing.
    pub fn display_title(&self) -> Cow<'_, str> {
        display_value(
            self.metadata.title.as_deref(),
            self.path.file_stem().and_then(OsStr::to_str),
            "Unknown title",
        )
    }

    /// Return the track artist tag, then the album artist tag, or the artist
    /// folder when the path follows the common `Artist/Album/track` layout.
    pub fn display_artist(&self) -> Cow<'_, str> {
        let artist_folder = artist_folder(&self.path);
        let artist = non_empty(self.metadata.artist.as_deref())
            .or_else(|| non_empty(self.metadata.album_artist.as_deref()));
        display_value(artist, artist_folder, "Unknown artist")
    }

    /// Return the album artist tag, falling back to the track artist and then
    /// the artist folder when the path follows `Artist/Album/track`.
    pub fn display_album_artist(&self) -> Cow<'_, str> {
        let artist_folder = artist_folder(&self.path);
        let artist = non_empty(self.metadata.album_artist.as_deref())
            .or_else(|| non_empty(self.metadata.artist.as_deref()));
        display_value(artist, artist_folder, "Unknown artist")
    }
    /// Return the album tag, or the containing folder as a display fallback.
    pub fn display_album(&self) -> Cow<'_, str> {
        display_value(
            self.metadata.album.as_deref(),
            self.path
                .parent()
                .and_then(Path::file_name)
                .and_then(OsStr::to_str),
            "Unknown album",
        )
    }

    pub(super) fn replace_metadata(&mut self, metadata: AudioMetadata) {
        self.metadata = metadata;
    }
}

fn artist_folder(path: &Path) -> Option<&str> {
    path.parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(OsStr::to_str)
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}
fn display_value<'a>(
    metadata_value: Option<&'a str>,
    path_value: Option<&'a str>,
    fallback: &'static str,
) -> Cow<'a, str> {
    metadata_value
        .filter(|value| !value.trim().is_empty())
        .or_else(|| path_value.filter(|value| !value.trim().is_empty()))
        .map(Cow::Borrowed)
        .unwrap_or(Cow::Borrowed(fallback))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn track(path: &str, metadata: AudioMetadata) -> Track {
        Track::new(TrackId::new(1), PathBuf::from(path), metadata)
    }

    #[test]
    fn track_stores_id_path_and_metadata() {
        let metadata = AudioMetadata {
            title: Some("Song title".to_owned()),
            duration: Some(Duration::from_secs(123)),
            ..AudioMetadata::default()
        };
        let track = track("Music/Artist/Album/song.flac", metadata);

        assert_eq!(track.id(), TrackId::new(1));
        assert_eq!(track.path(), Path::new("Music/Artist/Album/song.flac"));
        assert_eq!(track.metadata().title.as_deref(), Some("Song title"));
        assert_eq!(track.metadata().duration, Some(Duration::from_secs(123)));
    }

    #[test]
    fn display_title_prefers_metadata() {
        let track = track(
            "Music/Artist/Album/file-name.flac",
            AudioMetadata {
                title: Some("Tagged title".to_owned()),
                ..AudioMetadata::default()
            },
        );

        assert_eq!(track.display_title(), "Tagged title");
    }

    #[test]
    fn display_title_falls_back_to_file_stem() {
        let track = track(
            "Music/Artist/Album/file-name.flac",
            AudioMetadata::default(),
        );

        assert_eq!(track.display_title(), "file-name");
    }

    #[test]
    fn display_artist_falls_back_to_artist_folder() {
        let track = track("Music/Artist/Album/song.flac", AudioMetadata::default());

        assert_eq!(track.display_artist(), "Artist");
    }

    #[test]
    fn display_album_falls_back_to_parent_folder() {
        let track = track("Music/Artist/Album/song.flac", AudioMetadata::default());

        assert_eq!(track.display_album(), "Album");
    }

    #[test]
    fn display_artist_skips_empty_tag_and_uses_album_artist() {
        let track = track(
            "Music/Artist/Album/song.flac",
            AudioMetadata {
                artist: Some("  ".to_owned()),
                album_artist: Some("Album Artist".to_owned()),
                ..AudioMetadata::default()
            },
        );

        assert_eq!(track.display_artist(), "Album Artist");
        assert_eq!(track.display_album_artist(), "Album Artist");
    }

    #[test]
    fn display_album_artist_prefers_album_artist_to_track_artist() {
        let track = track(
            "Music/Artist/Album/song.flac",
            AudioMetadata {
                artist: Some("Track Artist".to_owned()),
                album_artist: Some("Album Artist".to_owned()),
                ..AudioMetadata::default()
            },
        );

        assert_eq!(track.display_artist(), "Track Artist");
        assert_eq!(track.display_album_artist(), "Album Artist");
    }
}
