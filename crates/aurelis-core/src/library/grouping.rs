use std::{cmp::Ordering, collections::BTreeMap};

use super::{Track, TrackId};

/// An album grouping built from a library's current tracks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Album {
    title: String,
    album_artist: String,
    track_ids: Vec<TrackId>,
}

impl Album {
    /// Album title, using the track's existing metadata and path fallbacks.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Effective album artist. This uses the album-artist tag first, then the
    /// track artist and the existing artist-folder fallback.
    pub fn album_artist(&self) -> &str {
        &self.album_artist
    }

    /// Track IDs ordered by disc, track number, and path.
    pub fn track_ids(&self) -> &[TrackId] {
        &self.track_ids
    }
}

/// An artist grouping built from a library's current tracks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Artist {
    name: String,
    track_ids: Vec<TrackId>,
}

impl Artist {
    /// Artist name, using the track artist and existing album-artist/path fallbacks.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Track IDs ordered by album, disc, track number, and path.
    pub fn track_ids(&self) -> &[TrackId] {
        &self.track_ids
    }
}

struct AlbumBuilder<'a> {
    title: String,
    album_artist: String,
    tracks: Vec<&'a Track>,
}

struct ArtistBuilder<'a> {
    name: String,
    tracks: Vec<&'a Track>,
}

pub(super) fn albums(tracks: &[Track]) -> Vec<Album> {
    let mut groups: BTreeMap<(String, String), AlbumBuilder<'_>> = BTreeMap::new();

    for track in tracks {
        let title = track.display_album().trim().to_owned();
        let album_artist = track.display_album_artist().trim().to_owned();
        let key = (normalize(&title), normalize(&album_artist));
        let group = groups.entry(key).or_insert_with(|| AlbumBuilder {
            title: title.clone(),
            album_artist: album_artist.clone(),
            tracks: Vec::new(),
        });

        if title < group.title {
            group.title = title;
        }
        if album_artist < group.album_artist {
            group.album_artist = album_artist;
        }
        group.tracks.push(track);
    }

    groups
        .into_values()
        .map(|mut group| {
            group
                .tracks
                .sort_by(|left, right| compare_disc_track(left, right));
            Album {
                title: group.title,
                album_artist: group.album_artist,
                track_ids: group.tracks.iter().map(|track| track.id()).collect(),
            }
        })
        .collect()
}

pub(super) fn artists(tracks: &[Track]) -> Vec<Artist> {
    let mut groups: BTreeMap<String, ArtistBuilder<'_>> = BTreeMap::new();

    for track in tracks {
        let name = track.display_artist().trim().to_owned();
        let key = normalize(&name);
        let group = groups.entry(key).or_insert_with(|| ArtistBuilder {
            name: name.clone(),
            tracks: Vec::new(),
        });

        if name < group.name {
            group.name = name;
        }
        group.tracks.push(track);
    }

    groups
        .into_values()
        .map(|mut group| {
            group
                .tracks
                .sort_by(|left, right| compare_artist_tracks(left, right));
            Artist {
                name: group.name,
                track_ids: group.tracks.iter().map(|track| track.id()).collect(),
            }
        })
        .collect()
}

pub(super) fn tracks_by_album<'a>(
    tracks: &'a [Track],
    title: &str,
    album_artist: &str,
) -> Vec<&'a Track> {
    let title = normalize(title);
    let album_artist = normalize(album_artist);
    let mut matching: Vec<_> = tracks
        .iter()
        .filter(|track| {
            normalize(&track.display_album()) == title
                && normalize(&track.display_album_artist()) == album_artist
        })
        .collect();
    matching.sort_by(|left, right| compare_disc_track(left, right));
    matching
}

pub(super) fn tracks_by_artist<'a>(tracks: &'a [Track], artist: &str) -> Vec<&'a Track> {
    let artist = normalize(artist);
    let mut matching: Vec<_> = tracks
        .iter()
        .filter(|track| normalize(&track.display_artist()) == artist)
        .collect();
    matching.sort_by(|left, right| compare_artist_tracks(left, right));
    matching
}

fn normalize(value: &str) -> String {
    value.trim().to_lowercase()
}

fn compare_artist_tracks(left: &Track, right: &Track) -> Ordering {
    normalize(&left.display_album())
        .cmp(&normalize(&right.display_album()))
        .then_with(|| compare_disc_track(left, right))
}

fn compare_disc_track(left: &Track, right: &Track) -> Ordering {
    compare_known_first(&left.metadata().disc, &right.metadata().disc)
        .then_with(|| compare_known_first(&left.metadata().track, &right.metadata().track))
        .then_with(|| left.path().cmp(right.path()))
        .then_with(|| left.id().cmp(&right.id()))
}

fn compare_known_first<T: Ord>(left: &Option<T>, right: &Option<T>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use crate::{audio::AudioMetadata, library::MusicLibrary};

    use super::{Album, Artist};

    fn add_track(
        library: &mut MusicLibrary,
        path: &str,
        title: Option<&str>,
        artist: Option<&str>,
        album: Option<&str>,
        album_artist: Option<&str>,
        disc: Option<u32>,
        track_number: Option<u32>,
    ) -> crate::library::TrackId {
        library.add_track(
            path,
            AudioMetadata {
                title: title.map(str::to_owned),
                artist: artist.map(str::to_owned),
                album: album.map(str::to_owned),
                album_artist: album_artist.map(str::to_owned),
                disc,
                track: track_number,
                ..AudioMetadata::default()
            },
        )
    }

    fn album<'a>(albums: &'a [Album], artist: &str) -> &'a Album {
        albums
            .iter()
            .find(|album| album.album_artist().eq_ignore_ascii_case(artist))
            .expect("album group exists")
    }

    fn artist<'a>(artists: &'a [Artist], name: &str) -> &'a Artist {
        artists
            .iter()
            .find(|artist| artist.name().eq_ignore_ascii_case(name))
            .expect("artist group exists")
    }

    #[test]
    fn compilation_tracks_group_by_album_artist_and_order_by_disc_and_track() {
        let mut library = MusicLibrary::new();
        let track_three = add_track(
            &mut library,
            "Music/Compilation/03.flac",
            Some("Three"),
            Some("Singer B"),
            Some("Compilation"),
            Some("Various Artists"),
            Some(1),
            Some(3),
        );
        let track_one = add_track(
            &mut library,
            "Music/Compilation/01.flac",
            Some("One"),
            Some("Singer A"),
            Some("Compilation"),
            Some("Various Artists"),
            Some(1),
            Some(1),
        );
        let disc_two = add_track(
            &mut library,
            "Music/Compilation/Disc 2/01.flac",
            Some("Disc Two"),
            Some("Singer C"),
            Some("Compilation"),
            Some("Various Artists"),
            Some(2),
            Some(1),
        );
        add_track(
            &mut library,
            "Music/Other/01.flac",
            Some("Other Album"),
            Some("Singer D"),
            Some("Compilation"),
            Some("Another Artist"),
            Some(1),
            Some(1),
        );

        let albums = library.albums();
        assert_eq!(albums.len(), 2);
        let compilation = album(&albums, "various artists");
        assert_eq!(compilation.title(), "Compilation");
        assert_eq!(compilation.track_ids(), [track_one, track_three, disc_two]);

        let matching = library.tracks_by_album("COMPILATION", "VARIOUS ARTISTS");
        assert_eq!(
            matching
                .iter()
                .map(|track| track.display_title().into_owned())
                .collect::<Vec<_>>(),
            ["One", "Three", "Disc Two"]
        );
    }

    #[test]
    fn artists_group_case_insensitively_and_order_tracks_deterministically() {
        let mut library = MusicLibrary::new();
        let second = add_track(
            &mut library,
            "Music/Artist/Z Album/02.flac",
            Some("Second"),
            Some("Example Artist"),
            Some("Z Album"),
            None,
            Some(1),
            Some(2),
        );
        let first = add_track(
            &mut library,
            "Music/Artist/Z Album/01.flac",
            Some("First"),
            Some("example artist"),
            Some("Z Album"),
            None,
            Some(1),
            Some(1),
        );
        let other_album = add_track(
            &mut library,
            "Music/Artist/A Album/01.flac",
            Some("Another Album"),
            Some("EXAMPLE ARTIST"),
            Some("A Album"),
            None,
            Some(1),
            Some(1),
        );

        let artists = library.artists();
        assert_eq!(artists.len(), 1);
        assert_eq!(
            artist(&artists, "Example Artist").track_ids(),
            [other_album, first, second]
        );
        assert_eq!(
            library
                .tracks_by_artist("example artist")
                .iter()
                .map(|track| track.display_title().into_owned())
                .collect::<Vec<_>>(),
            ["Another Album", "First", "Second"]
        );
    }

    #[test]
    fn missing_metadata_groups_by_artist_and_album_path_fallbacks() {
        let mut library = MusicLibrary::new();
        let second = add_track(
            &mut library,
            "Music/Artist/Album/02.flac",
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let first = add_track(
            &mut library,
            "Music/Artist/Album/01.flac",
            None,
            None,
            None,
            None,
            None,
            None,
        );

        let albums = library.albums();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].title(), "Album");
        assert_eq!(albums[0].album_artist(), "Artist");
        assert_eq!(albums[0].track_ids(), [first, second]);
        assert_eq!(library.artists()[0].name(), "Artist");
    }

    #[test]
    fn updating_a_duplicate_path_keeps_its_id_and_refreshes_grouping() {
        let mut library = MusicLibrary::new();
        let id = add_track(
            &mut library,
            "Music/Artist/Song.flac",
            Some("Song"),
            Some("Artist"),
            Some("Old Album"),
            None,
            None,
            None,
        );
        let updated_id = add_track(
            &mut library,
            "Music/Artist/Song.flac",
            Some("Song"),
            Some("Artist"),
            Some("New Album"),
            None,
            None,
            None,
        );

        assert_eq!(updated_id, id);
        assert_eq!(library.len(), 1);
        assert_eq!(
            library
                .albums()
                .iter()
                .map(Album::title)
                .collect::<Vec<_>>(),
            ["New Album"]
        );
    }
}
