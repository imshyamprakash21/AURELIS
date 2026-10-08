use std::{
    io::{self, BufRead, IsTerminal, Write},
    sync::mpsc,
    thread,
    time::Duration,
};

use aurelis_core::{
    audio::{AudioMetadata, PlaybackEndReason, PlaybackEngine},
    library::MusicLibrary,
};

mod cli;
mod library_browser;
mod library_play_test;
mod library_search;
mod library_test;
mod player_controller;
mod presentation;

use player_controller::PlaybackControlTarget;
use presentation::player::{
    finish_position_line, format_progress_line, format_time, print_controls, print_event,
    print_field, print_now_playing, print_player_screen, print_progress, print_queue,
    print_queue_position, print_track_details,
};

/* ============================================================
 * HELP / METADATA
 * ============================================================
 */

fn print_metadata(metadata: &AudioMetadata) {
    println!("AURELIS");
    println!("────────");
    println!();

    print_field("Title", metadata.title.as_deref());
    print_field("Artist", metadata.artist.as_deref());
    print_field("Album", metadata.album.as_deref());
    print_field("Genre", metadata.genre.as_deref());
    print_field("Year", metadata.year.as_deref());

    println!(
        "{:<12}  {}",
        "Track",
        metadata
            .track
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".to_string())
    );

    println!("{:<12}  {}", "Duration", metadata.duration_string());

    println!(
        "{:<12}  {}",
        "Codec",
        if metadata.codec.is_empty() {
            "Unknown"
        } else {
            &metadata.codec
        }
    );

    println!(
        "{:<12}  {}",
        "Sample Rate",
        metadata
            .sample_rate
            .map(|rate| format!("{rate} Hz"))
            .unwrap_or_else(|| "Unknown".to_string())
    );

    println!(
        "{:<12}  {}",
        "Bit Depth",
        metadata
            .bit_depth
            .map(|b| format!("{b}-bit"))
            .unwrap_or_else(|| "Unknown".to_string())
    );

    println!(
        "{:<12}  {}",
        "Channels",
        metadata
            .channels
            .map(|c| c.to_string())
            .unwrap_or_else(|| "Unknown".to_string())
    );
}

fn print_usage() {
    println!();
    println!("AURELIS");
    println!("────────────────────────────────────────────────────────");
    println!();
    println!("PLAYBACK");
    println!("  <file>                         Play an audio file");
    println!("  library-play <directory> <track-id> [queued-track-id ...]");
    println!("  library-play <directory> --all");
    println!();
    println!("LIBRARY");
    println!("  library <directory>            Browse the library");
    println!();
    println!("SEARCH & QUERY");
    println!("  library-search <directory> <query>");
    println!("  library-search-play <directory> <query> <result-number>");
    println!("  library-search-play-all <directory> <query>");
    println!("  library-query <directory> <query-options>");
    println!("  library-query-play <directory> <result-number> <query-options>");
    println!("  library-query-play-all <directory> <query-options>");
    println!("  Options: --search, --artist, --album, --genre, --year, --sort, --order");
    println!("  Sort keys: title, artist, album, year, track, duration, path");
    println!("  Sort order: asc or desc (default: title ascending)");
    println!();
    println!("INFORMATION");
    println!("  --help                         Show command help");
    println!("  --info <file>                  Show technical audio metadata");
    println!();
    println!("DEVELOPMENT / TEST");
    println!("  library-test <directory>       Run the interactive library diagnostics");
}

/* ============================================================
 * INPUT
 * ============================================================
 */

fn start_control_reader(command_tx: mpsc::Sender<String>) -> io::Result<()> {
    thread::Builder::new()
        .name("aurelis-cli-input".to_string())
        .spawn(move || {
            #[cfg(windows)]
            if io::stdin().is_terminal() {
                read_windows_console_keys(command_tx);

                return;
            }

            for line in io::stdin().lock().lines() {
                let Ok(line) = line else {
                    break;
                };

                let command = normalize_control_line(&line);

                if let Some(command) = command
                    && command_tx.send(command).is_err()
                {
                    break;
                }
            }
        })?;

    Ok(())
}

fn normalize_control_line(line: &str) -> Option<String> {
    let command = line.trim().to_ascii_lowercase();

    let normalized = match command.as_str() {
        "\u{1b}[d" | "\u{1b}od" | "left" => "left",

        "\u{1b}[c" | "\u{1b}oc" | "right" => "right",

        "" => return None,

        _ => return Some(command),
    };

    Some(normalized.to_string())
}

#[cfg(windows)]
fn read_windows_console_keys(command_tx: mpsc::Sender<String>) {
    use windows::Win32::System::Console::{
        GetStdHandle, INPUT_RECORD, KEY_EVENT, ReadConsoleInputW, STD_INPUT_HANDLE,
    };

    let Ok(input_handle) = (unsafe { GetStdHandle(STD_INPUT_HANDLE) }) else {
        return;
    };

    loop {
        let mut records = [INPUT_RECORD::default()];

        let mut count = 0;

        if unsafe { ReadConsoleInputW(input_handle, &mut records, &mut count) }.is_err() {
            return;
        }

        for record in records.iter().take(count as usize) {
            if record.EventType as u32 != KEY_EVENT {
                continue;
            }

            let key = unsafe { record.Event.KeyEvent };

            if !key.bKeyDown.as_bool() {
                continue;
            }

            let command = match key.wVirtualKeyCode {
                0x25 => "left".to_string(),

                0x27 => "right".to_string(),

                _ => {
                    let character = unsafe { key.uChar.UnicodeChar };

                    let Some(command) = char::from_u32(character as u32) else {
                        continue;
                    };

                    if command == '\0' {
                        continue;
                    }

                    command.to_ascii_lowercase().to_string()
                }
            };

            if command_tx.send(command).is_err() {
                return;
            }
        }
    }
}

/* ============================================================
 * PLAYBACK CONTROL LOOP
 * ============================================================
 */

fn run_playback_controls<T: PlaybackControlTarget>(
    engine: &mut T,
    library: Option<&MusicLibrary>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (command_tx, command_rx) = mpsc::channel::<String>();

    start_control_reader(command_tx)?;

    let mut observed_track_id = engine.current_track_id();

    let mut last_position_second = None;

    let mut position_line_width = 0;

    let mut position_line_cleared_near_end = false;

    /*
     * Initial screen.
     */
    if let Some(library) = library {
        if let Some(snapshot) = engine.playback_snapshot(Some(library)) {
            print_player_screen(library, &snapshot);
        }
    } else {
        println!();

        print_controls(false);

        println!();
    }

    loop {
        loop {
            match command_rx.try_recv() {
                Ok(command) => {
                    match command.as_str() {
                        /* --------------------
                         * PAUSE
                         * --------------------
                         */
                        "p" | "pause" => {
                            engine.pause();

                            let position = format_time(engine.position());

                            finish_position_line(position_line_width)?;

                            print_event(format!("Paused at {position}"));

                            position_line_width = 0;

                            last_position_second = None;
                        }

                        /* --------------------
                         * RESUME
                         * --------------------
                         */
                        "r" | "resume" => {
                            engine.resume();

                            let position = format_time(engine.position());

                            finish_position_line(position_line_width)?;

                            print_event(format!("Resumed at {position}"));

                            position_line_width = 0;

                            last_position_second = None;
                        }

                        /* --------------------
                         * CLEAR QUEUE
                         * --------------------
                         */
                        "c" | "clear" => match engine.clear_queue_entries() {
                            Ok(()) => {
                                finish_position_line(position_line_width)?;

                                print_event("Queue cleared. Current playback continues.");

                                position_line_width = 0;

                                last_position_second = None;
                            }

                            Err(error) => {
                                finish_position_line(position_line_width)?;

                                print_event(format!("Could not clear queue: {error}"));

                                position_line_width = 0;

                                last_position_second = None;
                            }
                        },

                        /* --------------------
                         * NEXT
                         * --------------------
                         */
                        "n" | "next" => {
                            finish_position_line(position_line_width)?;

                            match engine.next_track(library) {
                                Ok(Some(track_id)) => {
                                    print_event(format!("Next track → {track_id:?}"));

                                    observed_track_id = Some(track_id);

                                    position_line_width = 0;

                                    last_position_second = None;

                                    position_line_cleared_near_end = false;
                                }

                                Ok(None) => {
                                    print_event("No next track in the queue.");

                                    position_line_width = 0;

                                    last_position_second = None;
                                }

                                Err(error) => {
                                    print_event(format!("Could not start next track: {error}"));

                                    position_line_width = 0;

                                    last_position_second = None;
                                }
                            }
                        }

                        /* --------------------
                         * PREVIOUS
                         * --------------------
                         */
                        "b" | "previous" => {
                            finish_position_line(position_line_width)?;

                            match engine.previous_track(library) {
                                Ok(Some(track_id)) => {
                                    print_event(format!("Previous track → {track_id:?}"));

                                    observed_track_id = Some(track_id);

                                    position_line_width = 0;

                                    last_position_second = None;

                                    position_line_cleared_near_end = false;
                                }

                                Ok(None) => {
                                    print_event("Already at the first track in the queue.");

                                    position_line_width = 0;

                                    last_position_second = None;
                                }

                                Err(error) => {
                                    print_event(format!("Could not start previous track: {error}"));

                                    position_line_width = 0;

                                    last_position_second = None;
                                }
                            }
                        }

                        /* --------------------
                         * SEEK LEFT
                         * --------------------
                         */
                        "left" => {
                            if let Err(error) = engine.seek_by_seconds(-5) {
                                finish_position_line(position_line_width)?;

                                print_event(format!("Seek failed: {error}"));

                                position_line_width = 0;

                                last_position_second = None;
                            } else {
                                finish_position_line(position_line_width)?;

                                print_event("Seeked backward 5 seconds");

                                position_line_width = 0;

                                last_position_second = None;

                                position_line_cleared_near_end = false;
                            }
                        }

                        /* --------------------
                         * SEEK RIGHT
                         * --------------------
                         */
                        "right" => {
                            if let Err(error) = engine.seek_by_seconds(5) {
                                finish_position_line(position_line_width)?;

                                print_event(format!("Seek failed: {error}"));

                                position_line_width = 0;

                                last_position_second = None;
                            } else {
                                finish_position_line(position_line_width)?;

                                print_event("Seeked forward 5 seconds");

                                position_line_width = 0;

                                last_position_second = None;

                                position_line_cleared_near_end = false;
                            }
                        }

                        /* --------------------
                         * STATUS
                         * --------------------
                         */
                        "status" | "show" => {
                            finish_position_line(position_line_width)?;

                            if let Some(library) = library {
                                if let Some(snapshot) = engine.playback_snapshot(Some(library)) {
                                    print_track_details(library, &snapshot);
                                } else {
                                    print_event("Track details are unavailable.");
                                }
                            } else {
                                print_event("Technical details are available in library playback.");
                            }

                            position_line_width = 0;

                            last_position_second = None;
                        }

                        /* --------------------
                         * STOP
                         * --------------------
                         */
                        "s" | "stop" => {
                            engine.stop();

                            let position = format_time(engine.position());

                            finish_position_line(position_line_width)?;

                            print_event(format!("Playback stopped at {position}"));

                            return engine.wait().map(|_| ());
                        }

                        /* --------------------
                         * QUIT
                         * --------------------
                         */
                        "q" | "quit" => {
                            engine.stop();

                            let position = format_time(engine.position());

                            finish_position_line(position_line_width)?;

                            print_event(format!("Playback quit at {position}"));

                            return engine.wait().map(|_| ());
                        }

                        /* --------------------
                         * UNKNOWN
                         * --------------------
                         */
                        "" => {}

                        _ => {
                            finish_position_line(position_line_width)?;

                            if engine.queue_enabled() {
                                print_event(
                                    "Unknown command. Use ←, →, p, r, n, b, c, status, s, or q.",
                                );
                            } else {
                                print_event("Unknown command. Use ←, →, p, r, s, or q.");
                            }

                            position_line_width = 0;

                            last_position_second = None;
                        }
                    }
                }

                Err(mpsc::TryRecvError::Empty) => {
                    break;
                }

                Err(mpsc::TryRecvError::Disconnected) => {
                    break;
                }
            }
        }

        /* ====================================================
         * PLAYBACK STATE
         * ====================================================
         */

        let duration = engine.total_duration();

        let position = engine.position();

        let near_end = engine.queue_enabled()
            && duration.is_some_and(|duration| {
                position >= duration.saturating_sub(Duration::from_millis(100))
            });

        if near_end && !position_line_cleared_near_end {
            finish_position_line(position_line_width)?;

            position_line_width = 0;

            last_position_second = None;

            position_line_cleared_near_end = true;
        }

        /* ====================================================
         * NATURAL END / STOP
         * ====================================================
         */

        if let Some(result) = engine.poll(library) {
            finish_position_line(position_line_width)?;

            match &result {
                Ok(PlaybackEndReason::Completed) => {
                    print_event("Playback completed.");
                }

                Ok(PlaybackEndReason::Stopped) => {
                    print_event("Playback stopped.");
                }

                Err(error) => {
                    print_event(format!("Playback error: {error}"));
                }
            }

            return result.map(|_| ());
        }

        /* ====================================================
         * TRACK CHANGE
         * ====================================================
         */

        let current_track_id = engine.current_track_id();

        if current_track_id != observed_track_id {
            finish_position_line(position_line_width)?;

            if let Some(library) = library {
                if let Some(snapshot) = engine.playback_snapshot(Some(library)) {
                    print_event("Track changed.");

                    println!();

                    print_now_playing(library, &snapshot);

                    println!();

                    print_progress(&snapshot);

                    println!();

                    print_queue(library, &snapshot);

                    println!();

                    print_queue_position(library, &snapshot);

                    println!();
                }
            }

            observed_track_id = current_track_id;

            position_line_width = 0;

            last_position_second = None;

            position_line_cleared_near_end = false;
        }

        /* ====================================================
         * LIVE PROGRESS
         * ====================================================
         */

        let position = engine.position();

        if !position_line_cleared_near_end && last_position_second != Some(position.as_secs()) {
            let line = format_progress_line(position, engine.total_duration());

            print!(
                "\r{line:<width$}",
                width = position_line_width.max(line.chars().count(),)
            );

            io::stdout().flush()?;

            position_line_width = line.chars().count();

            last_position_second = Some(position.as_secs());
        }

        thread::sleep(Duration::from_millis(10));
    }
}

/* ============================================================
 * MAIN
 * ============================================================
 */

fn main() {
    aurelis_core::initialize();
    cli::run();
}

fn run_direct_playback(first: String) {
    let path = first;

    println!("Opening: {}\n", path);

    let metadata = match AudioMetadata::from_file(&path) {
        Ok(meta) => meta,

        Err(error) => {
            eprintln!("Metadata error: {error}");

            return;
        }
    };

    let mut engine = match PlaybackEngine::open(&path) {
        Ok(engine) => engine,

        Err(error) => {
            eprintln!("Failed to initialize playback: {error}");

            return;
        }
    };

    let output = engine.output_spec();

    println!("Now Playing");

    println!("────────────────────────");

    print_field("Title", metadata.title.as_deref());

    print_field("Artist", metadata.artist.as_deref());

    print_field("Album", metadata.album.as_deref());

    print_field("Genre", metadata.genre.as_deref());

    print_field("Year", metadata.year.as_deref());

    println!(
        "{:<12}: {}",
        "Track",
        metadata
            .track
            .map(|n| n.to_string())
            .unwrap_or_else(|| { "-".to_string() })
    );

    println!("{:<12}: {}", "Duration", metadata.duration_string());

    println!(
        "{:<12}: {}",
        "Codec",
        if metadata.codec.is_empty() {
            "Unknown"
        } else {
            &metadata.codec
        }
    );

    println!(
        "{:<12}: {}",
        "Sample Rate",
        metadata
            .sample_rate
            .map(|rate| { format!("{rate} Hz") })
            .unwrap_or_else(|| { "Unknown".to_string() })
    );

    println!(
        "{:<12}: {}",
        "Bit Depth",
        metadata
            .bit_depth
            .map(|b| { format!("{b}-bit") })
            .unwrap_or_else(|| { "Unknown".to_string() })
    );

    println!(
        "{:<12}: {}",
        "Channels",
        metadata
            .channels
            .map(|c| { c.to_string() })
            .unwrap_or_else(|| { "Unknown".to_string() })
    );

    println!();

    println!(
        "Output      : {} Hz | {} ch",
        output.sample_rate, output.channels
    );

    if engine.is_resampling() {
        println!(
            "Resampling  : Enabled ({} → {} Hz)",
            metadata.sample_rate.unwrap_or(0),
            output.sample_rate
        );
    } else {
        println!("Resampling  : Disabled in AURELIS (source and output rates match)");
    }

    if engine.is_channel_conversion() {
        println!(
            "Channels    : Converted ({} → {})",
            metadata.channels.unwrap_or(0),
            output.channels
        );
    } else {
        println!("Channels    : Native");
    }

    println!();

    if let Err(error) = engine.play() {
        eprintln!("Playback failed: {error}");

        return;
    }

    if let Err(error) = run_playback_controls(&mut engine, None) {
        eprintln!("Playback failed: {error}");
    }
}
/* ============================================================
 * TESTS
 * ============================================================
 */

#[cfg(test)]
mod presentation_tests {
    use super::presentation::player::{format_progress_line, truncate_text};

    use std::time::Duration;

    #[test]
    fn truncates_long_text() {
        let text = "This is a very long title";

        let result = truncate_text(text, 10);

        assert_eq!(result, "This is...");
    }

    #[test]
    fn keeps_short_text_unchanged() {
        let text = "sample3";

        assert_eq!(truncate_text(text, 20,), text);
    }

    #[test]
    fn progress_line_contains_time_and_bar() {
        let line = format_progress_line(Duration::from_secs(30), Some(Duration::from_secs(90)));

        assert!(line.contains("00:30"));

        assert!(line.contains("01:30"));

        assert!(line.contains("━"));

        assert!(line.contains("·"));
    }
}
