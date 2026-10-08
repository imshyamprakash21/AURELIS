use std::{
    io::{self, BufRead, IsTerminal},
    sync::mpsc,
    thread,
};

use aurelis_core::audio::AudioMetadata;

mod application;
mod cli;
mod library_browser;
mod library_play_test;
mod library_search;
mod library_test;
mod player_controller;
mod presentation;

use presentation::player::print_field;

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

/* ============================================================
 * MAIN
 * ============================================================
 */

fn main() {
    aurelis_core::initialize();
    cli::run();
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
