use std::cmp::Ordering;

use super::Track;

/// Field used to order matching tracks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SortKey {
    /// Track title, using [`Track::display_title`] fallbacks.
    #[default]
    Title,
    /// Artist, using [`Track::display_artist`] fallbacks.
    Artist,
    /// Album, using [`Track::display_album`] fallbacks.
    Album,
    /// The metadata year/date string.
    Year,
    /// The metadata track number.
    TrackNumber,
    /// Track duration.
    Duration,
    /// Stored path, compared lexicographically by the platform's `Path` order.
    Path,
}

/// Direction used when sorting query results.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SortOrder {
    /// Lowest or alphabetically first value first.
    #[default]
    Ascending,
    /// Highest or alphabetically last value first.
    Descending,
}

/// In-memory search, filter, and sort criteria for a [`MusicLibrary`](super::MusicLibrary).
///
/// All supplied criteria are combined with AND semantics. String filters use
/// exact Unicode lowercase matching; text search uses case-insensitive
/// substring matching over title, artist, album, album artist, and genre
/// metadata. Search also checks the existing title, artist, and album display
/// fallbacks. No fuzzy matching or ranking is performed. Text sort keys are
/// compared by lowercase value. For optional year, track number, and duration,
/// `None` sorts before `Some` in ascending order and after `Some` in descending
/// order. Equal sort values are always ordered by ascending `TrackId`.
#[derive(Clone, Debug, Default)]
pub struct TrackQuery {
    search: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    genre: Option<String>,
    year: Option<String>,
    sort_key: SortKey,
    sort_order: SortOrder,
}

impl TrackQuery {
    /// Create a query with no filters, title ascending sort.
    pub fn new() -> Self {
        Self::default()
    }

    /// Match a case-insensitive substring in searchable metadata.
    pub fn search(mut self, text: impl AsRef<str>) -> Self {
        self.search = Some(text.as_ref().to_lowercase());
        self
    }

    /// Require an exact, case-insensitive artist match.
    ///
    /// Uses the track's display artist, including its existing album-artist
    /// and path fallbacks when the corresponding tags are missing.
    pub fn artist(mut self, artist: impl AsRef<str>) -> Self {
        self.artist = Some(artist.as_ref().to_lowercase());
        self
    }

    /// Require an exact, case-insensitive album match.
    ///
    /// Uses the track's display album, including its existing path fallback
    /// when the album tag is missing.
    pub fn album(mut self, album: impl AsRef<str>) -> Self {
        self.album = Some(album.as_ref().to_lowercase());
        self
    }

    /// Require an exact, case-insensitive genre metadata match.
    pub fn genre(mut self, genre: impl AsRef<str>) -> Self {
        self.genre = Some(genre.as_ref().to_lowercase());
        self
    }

    /// Require an exact, case-insensitive year/date metadata match.
    ///
    /// Numeric years and strings are both accepted; matching is against the
    /// stored metadata string, so a date such as `2024-02-01` does not match
    /// the year filter `2024`.
    pub fn year(mut self, year: impl ToString) -> Self {
        self.year = Some(year.to_string().to_lowercase());
        self
    }

    /// Select the field used to sort matching tracks.
    pub fn sort_by(mut self, sort_key: SortKey) -> Self {
        self.sort_key = sort_key;
        self
    }

    /// Select ascending or descending result order.
    pub fn order(mut self, sort_order: SortOrder) -> Self {
        self.sort_order = sort_order;
        self
    }
}

pub(super) fn execute<'a>(tracks: &'a [Track], query: &TrackQuery) -> Vec<&'a Track> {
    let mut matching: Vec<_> = tracks
        .iter()
        .filter(|track| matches(track, query))
        .collect();

    matching.sort_by(|left, right| compare_tracks(left, right, query));
    matching
}

fn matches(track: &Track, query: &TrackQuery) -> bool {
    let metadata = track.metadata();

    query
        .search
        .as_deref()
        .is_none_or(|search| matches_search(track, search))
        && query
            .artist
            .as_deref()
            .is_none_or(|artist| track.display_artist().to_lowercase() == artist)
        && query
            .album
            .as_deref()
            .is_none_or(|album| track.display_album().to_lowercase() == album)
        && query.genre.as_deref().is_none_or(|genre| {
            metadata
                .genre
                .as_deref()
                .is_some_and(|value| value.to_lowercase() == genre)
        })
        && query.year.as_deref().is_none_or(|year| {
            metadata
                .year
                .as_deref()
                .is_some_and(|value| value.to_lowercase() == year)
        })
}

fn matches_search(track: &Track, search: &str) -> bool {
    let metadata = track.metadata();
    [
        metadata.title.as_deref(),
        metadata.artist.as_deref(),
        metadata.album.as_deref(),
        metadata.album_artist.as_deref(),
        metadata.genre.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|value| value.to_lowercase().contains(search))
        || track.display_title().to_lowercase().contains(search)
        || track.display_artist().to_lowercase().contains(search)
        || track.display_album().to_lowercase().contains(search)
}

fn compare_tracks(left: &Track, right: &Track, query: &TrackQuery) -> Ordering {
    let comparison = match query.sort_key {
        SortKey::Title => compare_folded(&left.display_title(), &right.display_title()),
        SortKey::Artist => compare_folded(&left.display_artist(), &right.display_artist()),
        SortKey::Album => compare_folded(&left.display_album(), &right.display_album()),
        SortKey::Year => compare_optional_folded(
            left.metadata().year.as_deref(),
            right.metadata().year.as_deref(),
        ),
        SortKey::TrackNumber => compare_optional(&left.metadata().track, &right.metadata().track),
        SortKey::Duration => {
            compare_optional(&left.metadata().duration, &right.metadata().duration)
        }
        SortKey::Path => left.path().cmp(right.path()),
    };

    let comparison = match query.sort_order {
        SortOrder::Ascending => comparison,
        SortOrder::Descending => comparison.reverse(),
    };

    comparison.then_with(|| left.id().cmp(&right.id()))
}

fn compare_folded(left: &str, right: &str) -> Ordering {
    left.to_lowercase().cmp(&right.to_lowercase())
}

fn compare_optional<T: Ord>(left: &Option<T>, right: &Option<T>) -> Ordering {
    left.cmp(right)
}

fn compare_optional_folded(left: Option<&str>, right: Option<&str>) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left), Some(right)) => compare_folded(left, right),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        audio::AudioMetadata,
        library::{MusicLibrary, TrackId},
    };
    use std::{path::Path, time::Duration};

    fn add_track(library: &mut MusicLibrary, path: &str, metadata: AudioMetadata) -> TrackId {
        library.add_track(path, metadata)
    }

    fn metadata(
        title: Option<&str>,
        artist: Option<&str>,
        album: Option<&str>,
        album_artist: Option<&str>,
        genre: Option<&str>,
        year: Option<&str>,
        track: Option<u32>,
        duration_secs: Option<u64>,
    ) -> AudioMetadata {
        AudioMetadata {
            title: title.map(str::to_owned),
            artist: artist.map(str::to_owned),
            album: album.map(str::to_owned),
            album_artist: album_artist.map(str::to_owned),
            genre: genre.map(str::to_owned),
            year: year.map(str::to_owned),
            track,
            duration: duration_secs.map(Duration::from_secs),
            ..AudioMetadata::default()
        }
    }

    fn query_titles(library: &MusicLibrary, query: &TrackQuery) -> Vec<String> {
        library
            .query(query)
            .iter()
            .map(|track| track.display_title().into_owned())
            .collect()
    }

    fn sorting_library() -> MusicLibrary {
        let mut library = MusicLibrary::new();
        add_track(
            &mut library,
            "z.wav",
            metadata(
                Some("Beta"),
                Some("Zed"),
                Some("Gamma"),
                None,
                Some("Pop"),
                Some("2002"),
                Some(2),
                Some(20),
            ),
        );
        add_track(
            &mut library,
            "m.wav",
            metadata(
                Some("Alpha"),
                Some("Able"),
                Some("Beta"),
                None,
                Some("Rock"),
                Some("2001"),
                Some(1),
                Some(10),
            ),
        );
        add_track(
            &mut library,
            "a.wav",
            metadata(
                Some("Charlie"),
                Some("Middle"),
                Some("Alpha"),
                None,
                Some("Jazz"),
                Some("2003"),
                Some(3),
                Some(30),
            ),
        );
        library
    }

    #[test]
    fn empty_query_returns_every_track() {
        let library = sorting_library();

        assert_eq!(
            query_titles(&library, &TrackQuery::new()),
            ["Alpha", "Beta", "Charlie"]
        );
    }

    #[test]
    fn search_matches_title_artist_album_album_artist_and_genre() {
        let cases = [
            ("Song love", None, None, None, None),
            ("Title", Some("Lovely Artist"), None, None, None),
            ("Title", None, Some("Love Album"), None, None),
            ("Title", None, None, Some("Love Group"), None),
            ("Title", None, None, None, Some("Love Pop")),
        ];

        for (index, (title, artist, album, album_artist, genre)) in cases.into_iter().enumerate() {
            let mut library = MusicLibrary::new();
            add_track(
                &mut library,
                &format!("{index}.wav"),
                metadata(
                    Some(title),
                    artist,
                    album,
                    album_artist,
                    genre,
                    None,
                    None,
                    None,
                ),
            );

            assert_eq!(
                library.query(&TrackQuery::new().search("love")).len(),
                1,
                "search should match case {index}"
            );
        }
    }

    #[test]
    fn search_is_case_insensitive_and_substring_based() {
        let mut library = MusicLibrary::new();
        add_track(
            &mut library,
            "song.wav",
            metadata(
                Some("A Wonderful Day"),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ),
        );

        assert_eq!(library.query(&TrackQuery::new().search("WONDER")).len(), 1);
        assert_eq!(library.query(&TrackQuery::new().search("absent")).len(), 0);
    }

    #[test]
    fn search_uses_unicode_aware_lowercase_matching() {
        let mut library = MusicLibrary::new();
        add_track(
            &mut library,
            "song.wav",
            metadata(Some("CAFÉ"), None, None, None, None, None, None, None),
        );

        assert_eq!(library.query(&TrackQuery::new().search("café")).len(), 1);
    }

    #[test]
    fn artist_filter_is_exact_and_case_insensitive() {
        let mut library = MusicLibrary::new();
        add_track(
            &mut library,
            "one.wav",
            metadata(
                Some("One"),
                Some("Justin Bieber"),
                None,
                None,
                None,
                None,
                None,
                None,
            ),
        );
        add_track(
            &mut library,
            "two.wav",
            metadata(
                Some("Two"),
                Some("Justin Bieber Jr"),
                None,
                None,
                None,
                None,
                None,
                None,
            ),
        );

        assert_eq!(
            query_titles(&library, &TrackQuery::new().artist("jUsTiN bIeBeR")),
            ["One"]
        );
    }

    #[test]
    fn album_filter_is_exact_and_case_insensitive() {
        let mut library = MusicLibrary::new();
        add_track(
            &mut library,
            "one.wav",
            metadata(
                Some("One"),
                None,
                Some("Believe"),
                None,
                None,
                None,
                None,
                None,
            ),
        );
        add_track(
            &mut library,
            "two.wav",
            metadata(
                Some("Two"),
                None,
                Some("Believe Deluxe"),
                None,
                None,
                None,
                None,
                None,
            ),
        );

        assert_eq!(
            query_titles(&library, &TrackQuery::new().album("BELIEVE")),
            ["One"]
        );
    }

    #[test]
    fn genre_filter_is_exact_and_case_insensitive() {
        let mut library = MusicLibrary::new();
        add_track(
            &mut library,
            "one.wav",
            metadata(Some("One"), None, None, None, Some("Pop"), None, None, None),
        );
        add_track(
            &mut library,
            "two.wav",
            metadata(
                Some("Two"),
                None,
                None,
                None,
                Some("Pop Rock"),
                None,
                None,
                None,
            ),
        );

        assert_eq!(
            query_titles(&library, &TrackQuery::new().genre("pOp")),
            ["One"]
        );
    }

    #[test]
    fn year_filter_matches_the_stored_date_string_exactly() {
        let mut library = MusicLibrary::new();
        add_track(
            &mut library,
            "one.wav",
            metadata(
                Some("One"),
                None,
                None,
                None,
                None,
                Some("2020"),
                None,
                None,
            ),
        );
        add_track(
            &mut library,
            "two.wav",
            metadata(
                Some("Two"),
                None,
                None,
                None,
                None,
                Some("2020-05-01"),
                None,
                None,
            ),
        );

        assert_eq!(
            query_titles(&library, &TrackQuery::new().year(2020)),
            ["One"]
        );
    }

    #[test]
    fn filters_combine_with_and_semantics() {
        let mut library = MusicLibrary::new();
        add_track(
            &mut library,
            "one.wav",
            metadata(
                Some("One"),
                Some("Justin Bieber"),
                Some("Believe"),
                None,
                Some("Pop"),
                Some("2012"),
                None,
                None,
            ),
        );
        add_track(
            &mut library,
            "two.wav",
            metadata(
                Some("Two"),
                Some("Justin Bieber"),
                Some("Believe"),
                None,
                Some("R&B"),
                Some("2012"),
                None,
                None,
            ),
        );

        assert_eq!(
            query_titles(
                &library,
                &TrackQuery::new()
                    .artist("Justin Bieber")
                    .album("Believe")
                    .genre("Pop")
                    .year("2012")
            ),
            ["One"]
        );
    }

    #[test]
    fn search_and_filters_combine_with_and_semantics() {
        let mut library = MusicLibrary::new();
        add_track(
            &mut library,
            "one.wav",
            metadata(
                Some("Love Yourself"),
                Some("Justin Bieber"),
                None,
                None,
                None,
                None,
                None,
                None,
            ),
        );
        add_track(
            &mut library,
            "two.wav",
            metadata(
                Some("Love Yourself"),
                Some("Another Artist"),
                None,
                None,
                None,
                None,
                None,
                None,
            ),
        );
        add_track(
            &mut library,
            "three.wav",
            metadata(
                Some("Purpose"),
                Some("Justin Bieber"),
                None,
                None,
                None,
                None,
                None,
                None,
            ),
        );

        assert_eq!(
            library
                .query(&TrackQuery::new().search("love").artist("Justin Bieber"))
                .len(),
            1
        );
    }

    #[test]
    fn title_sort_supports_ascending_and_descending_order() {
        let library = sorting_library();

        assert_eq!(
            query_titles(&library, &TrackQuery::new().sort_by(SortKey::Title)),
            ["Alpha", "Beta", "Charlie"]
        );
        assert_eq!(
            query_titles(
                &library,
                &TrackQuery::new()
                    .sort_by(SortKey::Title)
                    .order(SortOrder::Descending)
            ),
            ["Charlie", "Beta", "Alpha"]
        );
    }

    #[test]
    fn all_other_sort_keys_order_tracks_by_the_selected_field() {
        let library = sorting_library();
        let cases = [
            (SortKey::Artist, ["Alpha", "Charlie", "Beta"]),
            (SortKey::Album, ["Charlie", "Alpha", "Beta"]),
            (SortKey::Year, ["Alpha", "Beta", "Charlie"]),
            (SortKey::TrackNumber, ["Alpha", "Beta", "Charlie"]),
            (SortKey::Duration, ["Alpha", "Beta", "Charlie"]),
            (SortKey::Path, ["Charlie", "Alpha", "Beta"]),
        ];

        for (sort_key, expected) in cases {
            assert_eq!(
                query_titles(&library, &TrackQuery::new().sort_by(sort_key)),
                expected,
                "unexpected order for {sort_key:?}"
            );
        }
    }

    #[test]
    fn missing_sort_metadata_has_deterministic_option_order() {
        let mut library = MusicLibrary::new();
        add_track(
            &mut library,
            "missing.wav",
            metadata(Some("Missing"), None, None, None, None, None, None, None),
        );
        add_track(
            &mut library,
            "present-later.wav",
            metadata(
                Some("Later"),
                None,
                None,
                None,
                None,
                Some("2022"),
                Some(2),
                Some(20),
            ),
        );
        add_track(
            &mut library,
            "present-first.wav",
            metadata(
                Some("First"),
                None,
                None,
                None,
                None,
                Some("2020"),
                Some(1),
                Some(10),
            ),
        );

        for sort_key in [SortKey::Year, SortKey::TrackNumber, SortKey::Duration] {
            assert_eq!(
                query_titles(&library, &TrackQuery::new().sort_by(sort_key)),
                ["Missing", "First", "Later"],
                "None should sort first in ascending {sort_key:?} order"
            );
            assert_eq!(
                query_titles(
                    &library,
                    &TrackQuery::new()
                        .sort_by(sort_key)
                        .order(SortOrder::Descending)
                ),
                ["Later", "First", "Missing"],
                "None should sort last in descending {sort_key:?} order"
            );
        }
    }

    #[test]
    fn equal_sort_values_use_track_id_as_fixed_ascending_tie_breaker() {
        let mut library = MusicLibrary::new();
        let first_id = add_track(
            &mut library,
            "z.wav",
            metadata(Some("Same"), None, None, None, None, None, None, None),
        );
        let second_id = add_track(
            &mut library,
            "a.wav",
            metadata(Some("Same"), None, None, None, None, None, None, None),
        );

        for order in [SortOrder::Ascending, SortOrder::Descending] {
            let results = library.query(&TrackQuery::new().sort_by(SortKey::Title).order(order));
            assert_eq!(results[0].id(), first_id);
            assert_eq!(results[1].id(), second_id);
        }
    }

    #[test]
    fn query_returns_read_only_library_references_without_mutating_tracks() {
        let mut library = MusicLibrary::new();
        let id = add_track(
            &mut library,
            "song.wav",
            metadata(Some("Original"), None, None, None, None, None, None, None),
        );
        let original_len = library.len();
        let original_title = library.get(id).unwrap().metadata().title.clone();

        let results = library.query(&TrackQuery::new());

        assert!(std::ptr::eq(results[0], library.get(id).unwrap()));
        assert_eq!(library.len(), original_len);
        assert_eq!(library.get(id).unwrap().metadata().title, original_title);
        assert_eq!(Path::new("song.wav"), library.get(id).unwrap().path());
    }
}
