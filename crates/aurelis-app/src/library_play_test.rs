//! Temporary CLI harness for selecting a library track and playing it.

use std::{error::Error, path::Path};

use aurelis_core::library::{MusicLibrary, ScanIssue, Track, TrackId};

use crate::{player_controller::PlayerController, run_playback_controls};

pub(super) enum PlaySelection {
    All,
    Explicit {
        starting_id: String,
        queued_ids: Vec<String>,
    },
}

pub(super) fn run(root: &str, selection: PlaySelection) -> Result<(), Box<dyn Error>> {
    let root = Path::new(root);
    let mut library = MusicLibrary::new();
    let report = library.scan_directory(root);

    println!("AURELIS Library Playback Test");
    println!("==============================");
    println!("Root: {}", root.display());
    println!("Discovered: {}", report.discovered);
    println!("Added:      {}", report.added);
    println!("Updated:    {}", report.updated);
    println!("Failed:     {}", report.failed);
    println!("Issues:     {}", report.issues.len());

    for issue in &report.issues {
        match issue {
            ScanIssue::Discovery(issue) => {
                eprintln!("Discovery issue at {:?}: {}", issue.path, issue.message);
            }
            ScanIssue::Metadata(issue) => eprintln!("Metadata issue: {issue}"),
        }
    }

    println!();
    println!("Tracks:");
    for track in library.iter() {
        println!(
            "[{:?}] {} — {} — {}",
            track.id(),
            track.display_title(),
            track.display_artist(),
            track.display_album()
        );
    }

    if library.is_empty() {
        eprintln!(
            "No playable tracks were found in directory: {}.",
            root.display()
        );
        return Ok(());
    }

    let (track_id, resolved_queue) = match selection {
        PlaySelection::All => {
            let mut track_ids = all_track_ids(&library).into_iter();
            let track_id = track_ids
                .next()
                .expect("a non-empty library contains at least one track");
            (track_id, track_ids.collect::<Vec<_>>())
        }
        PlaySelection::Explicit {
            starting_id,
            queued_ids,
        } => {
            let Some(track_id) = resolve_track_id(&library, &starting_id) else {
                eprintln!("Track not found: {}", starting_id.trim());
                return Ok(());
            };
            if library.get(track_id).is_none() {
                eprintln!("Track not found: {}", starting_id.trim());
                return Ok(());
            }

            let mut resolved_queue = Vec::with_capacity(queued_ids.len());
            for requested in &queued_ids {
                let Some(queued_id) = resolve_track_id(&library, requested) else {
                    eprintln!("Track not found: {}", requested.trim());
                    return Ok(());
                };
                resolved_queue.push(queued_id);
            }

            (track_id, resolved_queue)
        }
    };

    let mut controller = PlayerController::new();
    if let Err(error) = controller.play_track(&library, track_id) {
        eprintln!("Unable to play selected track: {error}");
        return Ok(());
    }
    for queued_id in resolved_queue {
        if let Err(error) = controller.enqueue(&library, queued_id) {
            eprintln!("Unable to enqueue track: {error}");
            let _ = controller.shutdown();
            return Ok(());
        }
    }

    run_playback_controls(&mut controller, Some(&library))
}

fn all_track_ids(library: &MusicLibrary) -> Vec<TrackId> {
    library.iter().map(Track::id).collect()
}

fn resolve_track_id(library: &MusicLibrary, requested: &str) -> Option<TrackId> {
    let requested = requested.trim();
    // TrackId is intentionally opaque and currently has no public parser.
    // Match user input against IDs actually held by this library rather than
    // treating the numeric label as a collection index.
    library.iter().map(Track::id).find(|track_id| {
        let label = format!("{track_id:?}");
        label == requested
            || label
                .strip_prefix("TrackId(")
                .and_then(|value| value.strip_suffix(')'))
                .is_some_and(|number| number == requested)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aurelis_core::audio::AudioMetadata;

    #[test]
    fn resolves_the_displayed_track_id_and_numeric_short_form() {
        let mut library = MusicLibrary::new();
        let id = library.add_track("music/song.flac", AudioMetadata::default());

        assert_eq!(resolve_track_id(&library, &format!("{id:?}")), Some(id));
        assert_eq!(resolve_track_id(&library, "1"), Some(id));
    }

    #[test]
    fn rejects_track_ids_not_in_the_library() {
        let mut library = MusicLibrary::new();
        library.add_track("music/song.flac", AudioMetadata::default());

        assert_eq!(resolve_track_id(&library, "TrackId(999)"), None);
        assert_eq!(resolve_track_id(&library, "999"), None);
    }
}

#[cfg(test)]
mod all_track_queue_tests {
    use super::all_track_ids;
    use aurelis_core::{audio::AudioMetadata, library::MusicLibrary};

    #[test]
    fn all_track_queue_follows_library_order() {
        let mut library = MusicLibrary::new();
        let first = library.add_track("music/z-first.flac", AudioMetadata::default());
        let second = library.add_track("music/a-second.flac", AudioMetadata::default());
        let third = library.add_track("music/m-third.flac", AudioMetadata::default());

        assert_eq!(all_track_ids(&library), vec![first, second, third]);
    }
}
