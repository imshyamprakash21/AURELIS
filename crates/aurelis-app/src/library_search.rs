//! Search, query, result selection, and query-based playback CLI workflows.

use std::{fmt::Write as _, io::ErrorKind, path::Path};

use aurelis_core::library::{
    MusicLibrary, ScanIssue, ScanReport, SortKey, SortOrder, Track, TrackId, TrackQuery,
};

use crate::presentation::player::{BOX_INNER, display_width, pad_text_right, truncate_text};
use crate::{
    application::player::run_playback_controls, library_browser::push_table_border,
    player_controller::PlayerController,
};

const SEARCH_COLUMN_WIDTHS: [usize; 4] = [4, 24, 18, 13];

#[derive(Debug)]
pub(super) enum SearchCommand {
    Search {
        root: String,
        text: String,
    },
    SearchPlay {
        root: String,
        text: String,
        result_number: usize,
    },
    SearchPlayAll {
        root: String,
        text: String,
    },
    Query {
        root: String,
        options: QueryOptions,
    },
    QueryPlay {
        root: String,
        result_number: usize,
        options: QueryOptions,
    },
    QueryPlayAll {
        root: String,
        options: QueryOptions,
    },
}

#[derive(Clone, Debug, Default)]
pub(super) struct QueryOptions {
    search: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    genre: Option<String>,
    year: Option<String>,
    sort_key: Option<SortKey>,
    sort_order: Option<SortOrder>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlaybackSelection {
    Result(usize),
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResultSelectionError {
    NoResults,
    Zero,
    OutOfRange { requested: usize, total: usize },
}

struct QueryResults {
    library: MusicLibrary,
    report: ScanReport,
    track_ids: Vec<TrackId>,
}

pub(super) fn is_command_candidate(name: &str) -> bool {
    name.starts_with("library-search") || name.starts_with("library-query")
}

pub(super) fn parse_command(name: &str, arguments: Vec<String>) -> Result<SearchCommand, String> {
    if !matches!(
        name,
        "library-search"
            | "library-search-play"
            | "library-search-play-all"
            | "library-query"
            | "library-query-play"
            | "library-query-play-all"
    ) {
        return Err(format!("unknown command '{name}'"));
    }

    let mut arguments = arguments.into_iter();
    let root = arguments
        .next()
        .ok_or_else(|| format!("Usage: {}", command_usage(name)))?;

    match name {
        "library-search" => {
            let text = join_required_text(arguments.collect(), name)?;
            Ok(SearchCommand::Search { root, text })
        }
        "library-search-play" => {
            let mut values = arguments.collect::<Vec<_>>();
            if values.len() < 2 {
                return Err(format!("Usage: {}", command_usage(name)));
            }

            let result_number = parse_result_number(
                values
                    .pop()
                    .expect("the argument count was checked above")
                    .as_str(),
            )?;
            let text = join_required_text(values, name)?;
            Ok(SearchCommand::SearchPlay {
                root,
                text,
                result_number,
            })
        }
        "library-search-play-all" => {
            let text = join_required_text(arguments.collect(), name)?;
            Ok(SearchCommand::SearchPlayAll { root, text })
        }
        "library-query" => Ok(SearchCommand::Query {
            root,
            options: QueryOptions::parse(arguments.collect())?,
        }),
        "library-query-play" => {
            let result_number = arguments
                .next()
                .ok_or_else(|| format!("Usage: {}", command_usage(name)))?;
            let result_number = parse_result_number(&result_number)?;
            Ok(SearchCommand::QueryPlay {
                root,
                result_number,
                options: QueryOptions::parse(arguments.collect())?,
            })
        }
        "library-query-play-all" => Ok(SearchCommand::QueryPlayAll {
            root,
            options: QueryOptions::parse(arguments.collect())?,
        }),
        _ => unreachable!("the command name was validated above"),
    }
}

impl QueryOptions {
    fn parse(arguments: Vec<String>) -> Result<Self, String> {
        if arguments.is_empty() {
            return Err(format!(
                "Provide at least one query option. {QUERY_OPTIONS_USAGE}"
            ));
        }

        let mut options = Self::default();
        let mut index = 0;

        while index < arguments.len() {
            let option = arguments[index].as_str();
            index += 1;

            let value = arguments
                .get(index)
                .ok_or_else(|| format!("Missing value for {option}. {QUERY_OPTIONS_USAGE}"))?;
            if value.starts_with("--") || value.trim().is_empty() {
                return Err(format!("Missing value for {option}. {QUERY_OPTIONS_USAGE}"));
            }

            match option {
                "--search" => set_once(&mut options.search, value.clone(), option)?,
                "--artist" => set_once(&mut options.artist, value.clone(), option)?,
                "--album" => set_once(&mut options.album, value.clone(), option)?,
                "--genre" => set_once(&mut options.genre, value.clone(), option)?,
                "--year" => set_once(&mut options.year, value.clone(), option)?,
                "--sort" => set_once(&mut options.sort_key, parse_sort_key(value)?, option)?,
                "--order" => set_once(&mut options.sort_order, parse_sort_order(value)?, option)?,
                _ => {
                    return Err(format!(
                        "Unknown query option '{option}'. {QUERY_OPTIONS_USAGE}"
                    ));
                }
            }

            index += 1;
        }

        Ok(options)
    }

    fn to_track_query(&self) -> TrackQuery {
        let mut query = TrackQuery::new();

        if let Some(search) = &self.search {
            query = query.search(search);
        }
        if let Some(artist) = &self.artist {
            query = query.artist(artist);
        }
        if let Some(album) = &self.album {
            query = query.album(album);
        }
        if let Some(genre) = &self.genre {
            query = query.genre(genre);
        }
        if let Some(year) = &self.year {
            query = query.year(year);
        }
        if let Some(sort_key) = self.sort_key {
            query = query.sort_by(sort_key);
        }
        if let Some(sort_order) = self.sort_order {
            query = query.order(sort_order);
        }

        query
    }

    fn description(&self) -> String {
        let mut parts = Vec::new();

        if let Some(search) = &self.search {
            parts.push(format!("search: {search}"));
        }
        if let Some(artist) = &self.artist {
            parts.push(format!("artist: {artist}"));
        }
        if let Some(album) = &self.album {
            parts.push(format!("album: {album}"));
        }
        if let Some(genre) = &self.genre {
            parts.push(format!("genre: {genre}"));
        }
        if let Some(year) = &self.year {
            parts.push(format!("year: {year}"));
        }
        if let Some(sort_key) = self.sort_key {
            parts.push(format!("sort: {}", sort_key_label(sort_key)));
        }
        if let Some(sort_order) = self.sort_order {
            parts.push(format!(
                "order: {}",
                match sort_order {
                    SortOrder::Ascending => "asc",
                    SortOrder::Descending => "desc",
                }
            ));
        }

        parts.join(", ")
    }
}

const QUERY_OPTIONS_USAGE: &str = "Options: --search <text> --artist <name> --album <name> --genre <name> --year <year> --sort <title|artist|album|year|track|duration|path> --order <asc|desc>";

fn set_once<T>(slot: &mut Option<T>, value: T, option: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("Option {option} may only be provided once."));
    }
    *slot = Some(value);
    Ok(())
}

fn parse_sort_key(value: &str) -> Result<SortKey, String> {
    match value.to_ascii_lowercase().as_str() {
        "title" => Ok(SortKey::Title),
        "artist" => Ok(SortKey::Artist),
        "album" => Ok(SortKey::Album),
        "year" => Ok(SortKey::Year),
        "track" => Ok(SortKey::TrackNumber),
        "duration" => Ok(SortKey::Duration),
        "path" => Ok(SortKey::Path),
        _ => Err(format!(
            "Unknown sort key '{value}'. Use title, artist, album, year, track, duration, or path."
        )),
    }
}

fn parse_sort_order(value: &str) -> Result<SortOrder, String> {
    match value.to_ascii_lowercase().as_str() {
        "asc" => Ok(SortOrder::Ascending),
        "desc" => Ok(SortOrder::Descending),
        _ => Err(format!("Unknown sort order '{value}'. Use asc or desc.")),
    }
}

fn sort_key_label(sort_key: SortKey) -> &'static str {
    match sort_key {
        SortKey::Title => "title",
        SortKey::Artist => "artist",
        SortKey::Album => "album",
        SortKey::Year => "year",
        SortKey::TrackNumber => "track",
        SortKey::Duration => "duration",
        SortKey::Path => "path",
    }
}

fn command_usage(name: &str) -> &'static str {
    match name {
        "library-search" => "library-search <directory> <query>",
        "library-search-play" => "library-search-play <directory> <query> <result-number>",
        "library-search-play-all" => "library-search-play-all <directory> <query>",
        "library-query" => "library-query <directory> <query-options>",
        "library-query-play" => "library-query-play <directory> <result-number> <query-options>",
        "library-query-play-all" => "library-query-play-all <directory> <query-options>",
        _ => "library-search <directory> <query>",
    }
}

fn join_required_text(arguments: Vec<String>, command: &str) -> Result<String, String> {
    let text = arguments.join(" ");
    if text.trim().is_empty() {
        return Err(format!("Usage: {}", command_usage(command)));
    }
    Ok(text)
}

fn parse_result_number(value: &str) -> Result<usize, String> {
    let number = value
        .parse::<usize>()
        .map_err(|_| format!("Invalid result number '{value}'. Enter a positive result number."))?;
    if number == 0 {
        return Err("Invalid result number '0'. Result numbers start at 1.".to_string());
    }
    Ok(number)
}

pub(super) fn run_command(command: SearchCommand) {
    match command {
        SearchCommand::Search { root, text } => {
            let query = TrackQuery::new().search(&text);
            run_query(&root, "Search", &text, &query, None);
        }
        SearchCommand::SearchPlay {
            root,
            text,
            result_number,
        } => {
            let query = TrackQuery::new().search(&text);
            run_query(
                &root,
                "Search",
                &text,
                &query,
                Some(PlaybackSelection::Result(result_number)),
            );
        }
        SearchCommand::SearchPlayAll { root, text } => {
            let query = TrackQuery::new().search(&text);
            run_query(&root, "Search", &text, &query, Some(PlaybackSelection::All));
        }
        SearchCommand::Query { root, options } => {
            let query = options.to_track_query();
            let description = options.description();
            run_query(&root, "Query", &description, &query, None);
        }
        SearchCommand::QueryPlay {
            root,
            result_number,
            options,
        } => {
            let query = options.to_track_query();
            let description = options.description();
            run_query(
                &root,
                "Query",
                &description,
                &query,
                Some(PlaybackSelection::Result(result_number)),
            );
        }
        SearchCommand::QueryPlayAll { root, options } => {
            let query = options.to_track_query();
            let description = options.description();
            run_query(
                &root,
                "Query",
                &description,
                &query,
                Some(PlaybackSelection::All),
            );
        }
    }
}

fn run_query(
    root: &str,
    query_label: &str,
    query_description: &str,
    query: &TrackQuery,
    playback_selection: Option<PlaybackSelection>,
) {
    let results = scan_and_query(root, query);
    print!(
        "{}",
        render_search_results(root, query_label, query_description, &results)
    );

    let Some(playback_selection) = playback_selection else {
        return;
    };

    match select_playback_ids(&results.track_ids, playback_selection) {
        Ok(track_ids) => play_tracks(&results.library, &track_ids),
        Err(ResultSelectionError::NoResults) => {}
        Err(ResultSelectionError::Zero) => {
            println!("Invalid result number. Result numbers start at 1.");
        }
        Err(ResultSelectionError::OutOfRange { requested, total }) => {
            println!("Invalid result number {requested}. Choose a number from 1 to {total}.");
        }
    }
}

fn scan_and_query(root: &str, query: &TrackQuery) -> QueryResults {
    let mut library = MusicLibrary::new();
    let report = library.scan_directory(root);
    let track_ids = matching_track_ids(&library, query);

    QueryResults {
        library,
        report,
        track_ids,
    }
}

fn matching_track_ids(library: &MusicLibrary, query: &TrackQuery) -> Vec<TrackId> {
    library.query(query).into_iter().map(Track::id).collect()
}

fn select_playback_ids(
    result_ids: &[TrackId],
    selection: PlaybackSelection,
) -> Result<Vec<TrackId>, ResultSelectionError> {
    if result_ids.is_empty() {
        return Err(ResultSelectionError::NoResults);
    }

    match selection {
        PlaybackSelection::All => Ok(result_ids.to_vec()),
        PlaybackSelection::Result(0) => Err(ResultSelectionError::Zero),
        PlaybackSelection::Result(result_number) => result_ids
            .get(result_number - 1)
            .copied()
            .map(|track_id| vec![track_id])
            .ok_or(ResultSelectionError::OutOfRange {
                requested: result_number,
                total: result_ids.len(),
            }),
    }
}

fn play_tracks(library: &MusicLibrary, track_ids: &[TrackId]) {
    let Some((&first_track_id, queued_track_ids)) = track_ids.split_first() else {
        return;
    };

    let mut controller = PlayerController::new();
    if let Err(error) = controller.play_track(library, first_track_id) {
        eprintln!("Unable to start playback: {error}");
        return;
    }

    if let Err(error) = controller.enqueue_tracks(library, queued_track_ids.iter().copied()) {
        eprintln!("Unable to queue matching tracks: {error}");
        let _ = controller.shutdown();
        return;
    }

    if let Err(error) = run_playback_controls(&mut controller, Some(library)) {
        eprintln!("Playback controls failed: {error}");
    }
}

fn render_search_results(
    root: &str,
    query_label: &str,
    query_description: &str,
    results: &QueryResults,
) -> String {
    let mut output = String::new();
    push_search_banner(&mut output);

    writeln!(&mut output).expect("writing to a String cannot fail");
    writeln!(&mut output, "Root: {root}").expect("writing to a String cannot fail");
    writeln!(&mut output, "{query_label}: {query_description}")
        .expect("writing to a String cannot fail");
    writeln!(&mut output).expect("writing to a String cannot fail");

    if let Some(message) = root_scan_error(&results.report, Path::new(root)) {
        writeln!(&mut output, "{message}").expect("writing to a String cannot fail");
        return output;
    }

    if results.track_ids.is_empty() {
        let message = if results.library.is_empty() {
            if results.report.discovered == 0 && results.report.issues.is_empty() {
                "No supported audio files were found."
            } else {
                "No readable audio tracks were found."
            }
        } else {
            "No matching tracks."
        };
        writeln!(&mut output, "{message}").expect("writing to a String cannot fail");
    } else {
        writeln!(&mut output, "Results: {}", results.track_ids.len())
            .expect("writing to a String cannot fail");
        writeln!(&mut output).expect("writing to a String cannot fail");
        push_table_border(&mut output, &SEARCH_COLUMN_WIDTHS, '┌', '┬', '┐');
        push_search_row(&mut output, "#", "TITLE", "ARTIST", "ALBUM");
        push_table_border(&mut output, &SEARCH_COLUMN_WIDTHS, '├', '┼', '┤');

        for (index, track_id) in results.track_ids.iter().enumerate() {
            let Some(track) = results.library.get(*track_id) else {
                continue;
            };
            push_search_row(
                &mut output,
                &format!("{}", index + 1),
                &track.display_title(),
                &track.display_artist(),
                &track.display_album(),
            );
        }

        push_table_border(&mut output, &SEARCH_COLUMN_WIDTHS, '└', '┴', '┘');
    }

    writeln!(&mut output).expect("writing to a String cannot fail");
    writeln!(
        &mut output,
        "Scanned: {}  ·  Readable: {}  ·  Metadata failures: {}",
        results.report.discovered,
        results.library.len(),
        results.report.failed
    )
    .expect("writing to a String cannot fail");

    if results.report.failed > 0 {
        writeln!(
            &mut output,
            "Unable to read metadata for {} file(s); those tracks were skipped.",
            results.report.failed
        )
        .expect("writing to a String cannot fail");
    }

    let nested_discovery_issues = results
        .report
        .issues
        .iter()
        .filter_map(|issue| match issue {
            ScanIssue::Discovery(issue) if issue.path != Path::new(root) => Some(issue),
            _ => None,
        })
        .count();
    if nested_discovery_issues > 0 {
        writeln!(
            &mut output,
            "Some library entries could not be scanned ({} issue(s)).",
            nested_discovery_issues
        )
        .expect("writing to a String cannot fail");
    }

    output
}

fn root_scan_error(report: &ScanReport, root: &Path) -> Option<&'static str> {
    let issue = report.issues.iter().find_map(|issue| match issue {
        ScanIssue::Discovery(issue) if issue.path == root => Some(issue),
        _ => None,
    })?;

    Some(match issue.kind {
        ErrorKind::NotFound => "Directory does not exist.",
        ErrorKind::PermissionDenied => "Access to the selected directory was denied.",
        ErrorKind::InvalidInput if issue.message.contains("not a directory") => {
            "The selected path is not a directory."
        }
        ErrorKind::InvalidInput if issue.message.contains("symbolic-link") => {
            "Symbolic-link directories are not followed."
        }
        _ => "The selected directory could not be read.",
    })
}

fn push_search_banner(output: &mut String) {
    writeln!(output, "╔{}╗", "═".repeat(BOX_INNER)).expect("writing to a String cannot fail");
    let title = "AURELIS SEARCH";
    let remaining = BOX_INNER.saturating_sub(display_width(title));
    writeln!(
        output,
        "║{}{}{}║",
        " ".repeat(remaining / 2),
        title,
        " ".repeat(remaining - remaining / 2)
    )
    .expect("writing to a String cannot fail");
    writeln!(output, "╚{}╝", "═".repeat(BOX_INNER)).expect("writing to a String cannot fail");
}

fn push_search_row(output: &mut String, number: &str, title: &str, artist: &str, album: &str) {
    let [number_width, title_width, artist_width, album_width] = SEARCH_COLUMN_WIDTHS;
    let number = truncate_text(number, number_width);
    let title = truncate_text(title, title_width);
    let artist = truncate_text(artist, artist_width);
    let album = truncate_text(album, album_width);

    writeln!(
        output,
        "│ {} │ {} │ {} │ {} │",
        pad_text_right(&number, number_width),
        pad_text_right(&title, title_width),
        pad_text_right(&artist, artist_width),
        pad_text_right(&album, album_width)
    )
    .expect("writing to a String cannot fail");
}

#[cfg(test)]
mod tests {
    use super::*;
    use aurelis_core::audio::AudioMetadata;
    use std::path::PathBuf;
    use unicode_width::UnicodeWidthStr;

    fn test_library() -> (MusicLibrary, TrackId, TrackId, TrackId) {
        let mut library = MusicLibrary::new();
        library.add_track("music/other/album/ignored.flac", AudioMetadata::default());
        let alpha = library.add_track(
            PathBuf::from("music/artist/album/alpha.flac"),
            AudioMetadata {
                title: Some("Alpha Song 界🙂".to_string()),
                artist: Some("Justin Bieber".to_string()),
                album: Some("Purpose".to_string()),
                genre: Some("Pop".to_string()),
                year: Some("2023".to_string()),
                codec: "FLAC".to_string(),
                ..AudioMetadata::default()
            },
        );
        let beta = library.add_track(
            PathBuf::from("music/artist/album/beta.flac"),
            AudioMetadata {
                title: Some("Beta Song".to_string()),
                artist: Some("Justin Bieber".to_string()),
                album: Some("Justice".to_string()),
                genre: Some("R&B".to_string()),
                year: Some("2021".to_string()),
                ..AudioMetadata::default()
            },
        );
        let gamma = library.add_track(
            PathBuf::from("music/other/album/gamma.flac"),
            AudioMetadata {
                title: Some("Gamma Song".to_string()),
                artist: Some("Other Artist".to_string()),
                album: Some("Purpose".to_string()),
                genre: Some("Pop".to_string()),
                year: Some("2023".to_string()),
                ..AudioMetadata::default()
            },
        );

        (library, alpha, beta, gamma)
    }

    fn parse_options(arguments: &[&str]) -> Result<QueryOptions, String> {
        QueryOptions::parse(arguments.iter().map(|value| (*value).to_string()).collect())
    }

    fn result_ids(library: &MusicLibrary, query: &TrackQuery) -> Vec<TrackId> {
        matching_track_ids(library, query)
    }

    #[test]
    fn parses_search_play_with_a_multiword_query_and_ordinal() {
        let command = parse_command(
            "library-search-play",
            ["C:/Music", "Justin", "Bieber", "3"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        )
        .unwrap();

        assert!(matches!(
            command,
            SearchCommand::SearchPlay {
                root,
                text,
                result_number: 3
            } if root == "C:/Music" && text == "Justin Bieber"
        ));
    }

    #[test]
    fn parses_search_and_general_query_commands() {
        assert!(matches!(
            parse_command(
                "library-search",
                vec!["C:/Music".to_string(), "Justin Bieber".to_string()]
            ),
            Ok(SearchCommand::Search { root, text })
                if root == "C:/Music" && text == "Justin Bieber"
        ));

        assert!(matches!(
            parse_command(
                "library-query-play-all",
                vec![
                    "C:/Music".to_string(),
                    "--artist".to_string(),
                    "Justin Bieber".to_string(),
                    "--sort".to_string(),
                    "title".to_string()
                ]
            ),
            Ok(SearchCommand::QueryPlayAll { root, .. }) if root == "C:/Music"
        ));
    }

    #[test]
    fn reports_missing_query_and_result_arguments() {
        assert!(
            parse_command("library-search", vec!["C:/Music".to_string()])
                .unwrap_err()
                .contains("Usage:")
        );
        assert!(
            parse_command(
                "library-search-play",
                vec!["C:/Music".to_string(), "Justin Bieber".to_string()]
            )
            .unwrap_err()
            .contains("Usage:")
        );
        assert!(
            parse_command("library-query", vec!["C:/Music".to_string()])
                .unwrap_err()
                .contains("query option")
        );
    }

    #[test]
    fn rejects_malformed_and_zero_result_numbers() {
        assert!(
            parse_result_number("three")
                .unwrap_err()
                .contains("Invalid result number")
        );
        assert!(parse_result_number("0").unwrap_err().contains("start at 1"));
        assert!(
            parse_command(
                "library-query-play",
                vec![
                    "C:/Music".to_string(),
                    "NaN".to_string(),
                    "--artist".to_string(),
                    "Justin Bieber".to_string()
                ]
            )
            .unwrap_err()
            .contains("Invalid result number")
        );
    }

    #[test]
    fn unknown_query_commands_are_rejected() {
        assert!(
            parse_command("library-search-plau", Vec::new())
                .unwrap_err()
                .contains("unknown command")
        );
    }

    #[test]
    fn query_options_use_track_query_filters_and_sorting() {
        let (library, alpha, _, _) = test_library();
        let options = parse_options(&[
            "--artist",
            "JUSTIN BIEBER",
            "--album",
            "purpose",
            "--year",
            "2023",
            "--sort",
            "title",
            "--order",
            "desc",
        ])
        .unwrap();

        assert_eq!(result_ids(&library, &options.to_track_query()), vec![alpha]);
        assert_eq!(
            options.description(),
            "artist: JUSTIN BIEBER, album: purpose, year: 2023, sort: title, order: desc"
        );
    }

    #[test]
    fn supports_every_track_query_sort_key() {
        for sort in [
            "title", "artist", "album", "year", "track", "duration", "path",
        ] {
            assert!(parse_options(&["--sort", sort]).is_ok(), "{sort}");
        }
        assert!(parse_options(&["--sort", "rating"]).is_err());
        assert!(parse_options(&["--order", "random"]).is_err());
    }

    #[test]
    fn query_result_ids_match_tracks_and_preserve_query_order() {
        let (library, alpha, beta, gamma) = test_library();
        let ids = result_ids(&library, &TrackQuery::new().search("song"));

        assert_eq!(ids, vec![alpha, beta, gamma]);
        assert!(ids.iter().all(|id| library.get(*id).is_some()));
    }

    #[test]
    fn result_number_selects_a_one_based_search_ordinal() {
        let (library, alpha, beta, _) = test_library();
        let ids = result_ids(&library, &TrackQuery::new().search("song"));

        assert_eq!(
            select_playback_ids(&ids, PlaybackSelection::Result(2)),
            Ok(vec![beta])
        );
        assert_eq!(format!("{beta:?}"), "TrackId(3)");
        assert_eq!(
            select_playback_ids(&ids, PlaybackSelection::Result(1)),
            Ok(vec![alpha])
        );
    }

    #[test]
    fn reports_zero_out_of_range_and_empty_search_results() {
        let (_, alpha, beta, _) = test_library();
        let ids = [alpha, beta];

        assert_eq!(
            select_playback_ids(&ids, PlaybackSelection::Result(0)),
            Err(ResultSelectionError::Zero)
        );
        assert_eq!(
            select_playback_ids(&ids, PlaybackSelection::Result(999)),
            Err(ResultSelectionError::OutOfRange {
                requested: 999,
                total: 2
            })
        );
        assert_eq!(
            select_playback_ids(&[], PlaybackSelection::Result(1)),
            Err(ResultSelectionError::NoResults)
        );
    }

    #[test]
    fn play_all_uses_query_order_and_library_owned_track_ids() {
        let (library, alpha, beta, gamma) = test_library();
        let ids = result_ids(&library, &TrackQuery::new().search("song"));
        let all_ids = select_playback_ids(&ids, PlaybackSelection::All).unwrap();

        assert_eq!(all_ids, vec![alpha, beta, gamma]);
        assert!(all_ids.iter().all(|id| library.get(*id).is_some()));
    }

    #[test]
    fn search_table_uses_unicode_cell_width_and_hides_audio_details() {
        let (library, _, _, _) = test_library();
        let query = TrackQuery::new().search("song");
        let ids = result_ids(&library, &query);
        let results = QueryResults {
            library,
            report: ScanReport {
                discovered: 4,
                added: 4,
                ..ScanReport::default()
            },
            track_ids: ids,
        };
        let rendered = render_search_results("C:/Music", "Search", "song", &results);

        for line in rendered.lines().filter(|line| {
            line.starts_with('╔')
                || line.starts_with('║')
                || line.starts_with('╚')
                || line.starts_with('┌')
                || line.starts_with('│')
                || line.starts_with('├')
                || line.starts_with('└')
        }) {
            assert_eq!(UnicodeWidthStr::width(line), BOX_INNER + 2, "{line:?}");
        }
        assert!(rendered.contains("Alpha Song"));
        assert!(!rendered.contains("FLAC"));
    }

    #[test]
    fn rejects_duplicate_query_options_and_unknown_options() {
        assert!(parse_options(&["--artist", "Justin", "--artist", "Bieber"]).is_err());
        assert!(parse_options(&["--mood", "chill"]).is_err());
    }
}
