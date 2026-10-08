use std::env;

use aurelis_core::audio::AudioMetadata;

use crate::{
    library_browser, library_play_test, library_search, library_test, print_metadata, print_usage,
    run_direct_playback,
};

#[derive(Debug, PartialEq, Eq)]
enum LibraryPlayArguments {
    All {
        root: String,
    },
    Explicit {
        root: String,
        track_id: String,
        queued_ids: Vec<String>,
    },
}

fn parse_library_play_arguments(
    args: impl IntoIterator<Item = String>,
) -> Result<LibraryPlayArguments, String> {
    let mut args = args.into_iter();
    let root = args
        .next()
        .ok_or_else(|| "library-play requires a directory path.".to_string())?;
    let track_id = args
        .next()
        .ok_or_else(|| "library-play requires a TrackId or --all.".to_string())?;
    let queued_ids = args.collect::<Vec<_>>();

    if track_id == "--all" {
        if !queued_ids.is_empty() {
            return Err(
                "--all must be used by itself after the directory; it cannot be combined with a TrackId or queued IDs."
                    .to_string(),
            );
        }

        return Ok(LibraryPlayArguments::All { root });
    }

    if queued_ids.iter().any(|queued_id| queued_id == "--all") {
        return Err(
            "--all must be used by itself after the directory; it cannot be combined with a TrackId or queued IDs."
                .to_string(),
        );
    }

    Ok(LibraryPlayArguments::Explicit {
        root,
        track_id,
        queued_ids,
    })
}

fn print_library_play_usage() {
    println!("Usage:");
    println!("  aurelis-app library-play <directory> <track-id> [queued-track-id ...]");
    println!("  aurelis-app library-play <directory> --all");
}

fn is_unknown_library_command(command: &str) -> bool {
    command.starts_with("library-") && std::path::Path::new(command).extension().is_none()
}

pub(super) fn run() {
    let mut args = env::args().skip(1);

    let first = match args.next() {
        Some(value) => value,

        None => {
            print_usage();
            return;
        }
    };

    if first == "--help" || first == "-h" {
        print_usage();
        return;
    }

    if first == "library" {
        let Some(root) = args.next() else {
            eprintln!("Error: library requires a directory path.");
            println!("Usage: aurelis-app library <directory>");
            return;
        };

        library_browser::run(&root);
        return;
    }

    if library_search::is_command_candidate(&first) {
        match library_search::parse_command(&first, args.collect()) {
            Ok(command) => library_search::run_command(command),
            Err(message) => println!("Error: {message}"),
        }
        return;
    }

    if first == "library-test" {
        let Some(root) = args.next() else {
            eprintln!("Error: library-test requires a directory path.");

            println!("Usage: aurelis-app library-test <directory>");

            return;
        };

        if let Err(error) = library_test::run(&root) {
            eprintln!("Library test failed: {error}");
        }

        return;
    }

    if first == "library-play" {
        match parse_library_play_arguments(args) {
            Ok(LibraryPlayArguments::All { root }) => {
                if let Err(error) =
                    library_play_test::run(&root, library_play_test::PlaySelection::All)
                {
                    eprintln!("Library playback failed: {error}");
                }
            }
            Ok(LibraryPlayArguments::Explicit {
                root,
                track_id,
                queued_ids,
            }) => {
                if let Err(error) = library_play_test::run(
                    &root,
                    library_play_test::PlaySelection::Explicit {
                        starting_id: track_id,
                        queued_ids,
                    },
                ) {
                    eprintln!("Library playback failed: {error}");
                }
            }
            Err(error) => {
                eprintln!("Error: {error}");
                print_library_play_usage();
            }
        }

        return;
    }

    if first == "--info" {
        let path = match args.next() {
            Some(path) => path,

            None => {
                eprintln!("Error: --info requires an audio file path.");

                print_usage();

                return;
            }
        };

        println!("Reading: {}\n", path);

        let metadata = match AudioMetadata::from_file(&path) {
            Ok(metadata) => metadata,

            Err(error) => {
                eprintln!("Metadata error: {error}");

                return;
            }
        };

        print_metadata(&metadata);

        return;
    }

    if is_unknown_library_command(&first) {
        println!("Error: unknown command '{first}'. Use --help to list available commands.");
        return;
    }

    run_direct_playback(first);
}
#[cfg(test)]
mod library_play_argument_tests {
    use super::{LibraryPlayArguments, is_unknown_library_command, parse_library_play_arguments};

    fn parse(args: &[&str]) -> Result<LibraryPlayArguments, String> {
        parse_library_play_arguments(args.iter().map(|argument| (*argument).to_string()))
    }

    #[test]
    fn unknown_library_commands_do_not_fall_through_to_file_playback() {
        assert!(is_unknown_library_command("library-search-paly"));
        assert!(!is_unknown_library_command("library-song.flac"));
        assert!(!is_unknown_library_command("C:/Music/song.flac"));
    }

    #[test]
    fn accepts_all_tracks_without_explicit_ids() {
        assert_eq!(
            parse(&["C:/Music", "--all"]),
            Ok(LibraryPlayArguments::All {
                root: "C:/Music".to_string(),
            })
        );
    }

    #[test]
    fn preserves_explicit_start_and_queued_ids() {
        assert_eq!(
            parse(&["C:/Music", "6", "1", "8"]),
            Ok(LibraryPlayArguments::Explicit {
                root: "C:/Music".to_string(),
                track_id: "6".to_string(),
                queued_ids: vec!["1".to_string(), "8".to_string()],
            })
        );
    }

    #[test]
    fn rejects_missing_directory_or_selection() {
        assert!(parse(&[]).unwrap_err().contains("directory path"));
        assert!(
            parse(&["C:/Music"])
                .unwrap_err()
                .contains("TrackId or --all")
        );
    }

    #[test]
    fn rejects_all_combined_with_explicit_or_queued_ids() {
        for args in [
            &["C:/Music", "--all", "1"][..],
            &["C:/Music", "1", "--all"][..],
        ] {
            assert!(
                parse(args)
                    .unwrap_err()
                    .contains("--all must be used by itself")
            );
        }
    }
}
