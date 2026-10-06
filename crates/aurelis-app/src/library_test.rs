//! Temporary manual harness for exercising the core music-library API.

use std::{
    io::{self, BufRead, Write},
    path::Path,
};

use aurelis_core::library::{MusicLibrary, ScanIssue, SortKey, SortOrder, Track, TrackQuery};

pub(super) fn run(root: impl AsRef<Path>) -> io::Result<()> {
    let root = root.as_ref();
    let mut library = MusicLibrary::new();

    println!("AURELIS LIBRARY MANUAL TEST");
    println!("===========================");
    println!();
    println!("Root: {}", root.display());

    let report = library.scan_directory(root);
    println!();
    println!("Discovered: {}", report.discovered);
    println!("Added:      {}", report.added);
    println!("Updated:    {}", report.updated);
    println!("Failed:     {}", report.failed);
    println!("Issues:     {}", report.issues.len());

    for issue in &report.issues {
        match issue {
            ScanIssue::Discovery(issue) => println!(
                "  Discovery: {:?}: {} ({:?})",
                issue.path, issue.message, issue.kind
            ),
            ScanIssue::Metadata(issue) => {
                println!("  Metadata: {:?}: {}", issue.path(), issue.source_error())
            }
        }
    }

    println!();
    println!("Tracks:");
    if library.is_empty() {
        println!("  No tracks were added.");
    } else {
        for track in library.iter() {
            print_track_details(track);
        }
    }

    let root_failed = report.discovered == 0
        && report.issues.iter().any(
            |issue| matches!(issue, ScanIssue::Discovery(discovery) if discovery.path == root),
        );
    if root_failed {
        println!();
        println!("Unable to scan the root directory; exiting the library test.");
        return Ok(());
    }

    print_help();
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut line = String::new();

    loop {
        print!("> ");
        io::stdout().flush()?;
        line.clear();

        if input.read_line(&mut line)? == 0 {
            println!();
            break;
        }

        let command_line = line.trim();
        if command_line.is_empty() {
            continue;
        }

        let (command, argument) = split_command(command_line);
        match command.to_ascii_lowercase().as_str() {
            "search" => {
                if require_argument("search", argument) {
                    let query = TrackQuery::new().search(argument);
                    print_query_results(&library, &query, &format!("SEARCH: {argument}"));
                }
            }
            "artist" => {
                if require_argument("artist", argument) {
                    let query = TrackQuery::new().artist(argument);
                    print_query_results(&library, &query, &format!("ARTIST: {argument}"));
                }
            }
            "album" => {
                if require_argument("album", argument) {
                    let query = TrackQuery::new().album(argument);
                    print_query_results(&library, &query, &format!("ALBUM: {argument}"));
                }
            }
            "genre" => {
                if require_argument("genre", argument) {
                    let query = TrackQuery::new().genre(argument);
                    print_query_results(&library, &query, &format!("GENRE: {argument}"));
                }
            }
            "year" => {
                if require_argument("year", argument) {
                    let query = TrackQuery::new().year(argument);
                    print_query_results(&library, &query, &format!("YEAR: {argument}"));
                }
            }
            "sort" => match parse_sort(argument) {
                Ok((sort_key, sort_order, label)) => {
                    let query = TrackQuery::new().sort_by(sort_key).order(sort_order);
                    print_sort_results(&library, &query, &label);
                }
                Err(message) => println!("{message}"),
            },
            "all" => {
                if !argument.is_empty() {
                    println!("Usage: all");
                } else {
                    print_query_results(&library, &TrackQuery::new(), "ALL");
                }
            }
            "help" => print_help(),
            "quit" | "exit" => break,
            _ => println!("Unknown command. Type 'help' to see available commands."),
        }
    }

    println!("Library test finished.");
    Ok(())
}

fn print_track_details(track: &Track) {
    let metadata = track.metadata();
    println!(
        "[{:?}] {} — {} — {}",
        track.id(),
        track.display_title(),
        track.display_artist(),
        track.display_album()
    );
    println!(
        "    Year: {}",
        metadata.year.as_deref().unwrap_or("Unknown")
    );
    println!(
        "    Genre: {}",
        metadata.genre.as_deref().unwrap_or("Unknown")
    );
    println!(
        "    Track: {}",
        metadata
            .track
            .map(|number| number.to_string())
            .unwrap_or_else(|| "-".to_owned())
    );
    println!("    Duration: {}", metadata.duration_string());
    println!("    Path: {}", track.path().display());
}

fn print_query_results(library: &MusicLibrary, query: &TrackQuery, heading: &str) {
    let tracks = library.query(query);
    println!();
    println!("{heading}");
    println!("Results: {}", tracks.len());

    if tracks.is_empty() {
        println!("No matching tracks.");
        return;
    }

    for track in tracks {
        println!(
            "[{:?}] {} — {} — {}",
            track.id(),
            track.display_title(),
            track.display_artist(),
            track.display_album()
        );
    }
}

fn print_sort_results(library: &MusicLibrary, query: &TrackQuery, label: &str) {
    let tracks = library.query(query);
    println!();
    println!("SORT: {label}");
    println!("Results: {}", tracks.len());

    if tracks.is_empty() {
        println!("No matching tracks.");
        return;
    }

    println!("TrackId | Title | Artist | Album | Year | Track");
    for track in tracks {
        let metadata = track.metadata();
        println!(
            "{:?} | {} | {} | {} | {} | {}",
            track.id(),
            track.display_title(),
            track.display_artist(),
            track.display_album(),
            metadata.year.as_deref().unwrap_or("-"),
            metadata
                .track
                .map(|number| number.to_string())
                .unwrap_or_else(|| "-".to_owned())
        );
    }
}

fn split_command(line: &str) -> (&str, &str) {
    match line.find(char::is_whitespace) {
        Some(index) => (&line[..index], line[index..].trim()),
        None => (line, ""),
    }
}

fn require_argument(command: &str, argument: &str) -> bool {
    if argument.is_empty() {
        println!("Usage: {command} <value>");
        false
    } else {
        true
    }
}

fn parse_sort(argument: &str) -> Result<(SortKey, SortOrder, String), String> {
    let mut parts = argument.split_whitespace();
    let Some(key) = parts.next() else {
        return Err("Usage: sort <title|artist|album|year|track|duration|path> <asc|desc>".into());
    };
    let Some(direction) = parts.next() else {
        return Err("Usage: sort <title|artist|album|year|track|duration|path> <asc|desc>".into());
    };
    if parts.next().is_some() {
        return Err("Usage: sort <title|artist|album|year|track|duration|path> <asc|desc>".into());
    }

    let (sort_key, key_label) = match key.to_ascii_lowercase().as_str() {
        "title" => (SortKey::Title, "Title"),
        "artist" => (SortKey::Artist, "Artist"),
        "album" => (SortKey::Album, "Album"),
        "year" => (SortKey::Year, "Year"),
        "track" => (SortKey::TrackNumber, "Track"),
        "duration" => (SortKey::Duration, "Duration"),
        "path" => (SortKey::Path, "Path"),
        _ => {
            return Err(format!(
                "Unknown sort key '{key}'. Type 'help' for valid keys."
            ));
        }
    };

    let (sort_order, order_label) = match direction.to_ascii_lowercase().as_str() {
        "asc" => (SortOrder::Ascending, "Ascending"),
        "desc" => (SortOrder::Descending, "Descending"),
        _ => return Err("Sort direction must be 'asc' or 'desc'.".into()),
    };

    Ok((sort_key, sort_order, format!("{key_label} {order_label}")))
}

fn print_help() {
    println!();
    println!("Commands:");
    println!("  search <text>");
    println!("  artist <name>");
    println!("  album <name>");
    println!("  genre <name>");
    println!("  year <value>");
    println!("  sort <title|artist|album|year|track|duration|path> <asc|desc>");
    println!("  all");
    println!("  help");
    println!("  quit");
    println!();
    println!("Examples: search justin | artist Justin Bieber | sort title asc");
}
