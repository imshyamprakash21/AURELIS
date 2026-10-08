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
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

mod cli;
mod library_browser;
mod library_play_test;
mod library_search;
mod library_test;
mod player_controller;

use player_controller::{PlaybackControlTarget, PlaybackSnapshot};

const BOX_WIDTH: usize = 72;
const BOX_INNER: usize = BOX_WIDTH - 2;

const LABEL_WIDTH: usize = 13;
const VALUE_WIDTH: usize = BOX_INNER - LABEL_WIDTH - 6;

const QUEUE_NO_WIDTH: usize = 5;
const QUEUE_TITLE_WIDTH: usize = 42;
const QUEUE_ID_WIDTH: usize = 15;

const PROGRESS_BAR_WIDTH: usize = 40;

fn print_field(label: &str, value: Option<&str>) {
    println!("{:<12}  {}", label, value.unwrap_or("Unknown"));
}

fn format_time(position: Duration) -> String {
    format!(
        "{:02}:{:02}",
        position.as_secs() / 60,
        position.as_secs() % 60
    )
}

fn truncate_text(value: &str, width: usize) -> String {
    let clean = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();

    if display_width(&clean) <= width {
        return clean;
    }

    if width <= 3 {
        return take_text_width(&clean, width);
    }

    format!("{}...", take_text_width(&clean, width - 3))
}

fn display_width(value: &str) -> usize {
    UnicodeWidthStr::width(value)
}

fn take_text_width(value: &str, width: usize) -> String {
    let mut result = String::new();
    let mut used_width = 0;

    for character in value.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used_width + character_width > width {
            break;
        }

        result.push(character);
        used_width += character_width;
    }

    result
}

fn pad_text_right(value: &str, width: usize) -> String {
    format!(
        "{value}{}",
        " ".repeat(width.saturating_sub(display_width(value)))
    )
}

fn pad_text_left(value: &str, width: usize) -> String {
    format!(
        "{}{value}",
        " ".repeat(width.saturating_sub(display_width(value)))
    )
}

fn print_event(message: impl std::fmt::Display) {
    println!();
    println!("  ◆  {message}");
}

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
} /* ============================================================
 * BOX PRIMITIVES
 * ============================================================
 */

fn print_player_header() {
    println!();
    println!("╭{}╮", "─".repeat(BOX_INNER));
    println!("│{:^inner$}│", "AURELIS  PLAYER", inner = BOX_INNER);
    println!("╰{}╯", "─".repeat(BOX_INNER));
}

fn print_section_header(title: &str) {
    let title = format!("  {title}  ");
    let remaining = BOX_INNER.saturating_sub(title.chars().count());

    println!("┌{}{}┐", title, "─".repeat(remaining));
}

fn print_section_bottom() {
    println!("└{}┘", "─".repeat(BOX_INNER));
}

fn print_section_row(label: &str, value: impl std::fmt::Display) {
    let value = truncate_text(&value.to_string(), VALUE_WIDTH);

    println!(
        "│ {}│ {} │",
        pad_text_right(label, LABEL_WIDTH),
        pad_text_right(&value, VALUE_WIDTH)
    );
}

fn print_section_divider(widths: &[usize]) {
    print!("├");
    for (index, width) in widths.iter().enumerate() {
        if index > 0 {
            print!("┼");
        }
        print!("{}", "─".repeat(*width + 2));
    }
    println!("┤");
}

/* ============================================================
 * NOW PLAYING
 * ============================================================
 */

fn playback_state_label(snapshot: &PlaybackSnapshot) -> &'static str {
    match snapshot.state() {
        player_controller::PlaybackStatus::Playing => "▶  Playing",
        player_controller::PlaybackStatus::Paused => "Ⅱ  Paused",
        player_controller::PlaybackStatus::Stopped => "■  Stopped",
        player_controller::PlaybackStatus::Finished => "✓  Finished",
    }
}

fn print_now_playing(library: &MusicLibrary, snapshot: &PlaybackSnapshot) {
    print_section_header("NOW PLAYING");

    match snapshot.current_track_id() {
        Some(track_id) => match library.get(track_id) {
            Some(track) => {
                print_section_row("Title", track.display_title());
                print_section_row("Artist", track.display_artist());
                print_section_row("Album", track.display_album());
                print_section_row("State", playback_state_label(snapshot));
            }
            None => {
                print_section_row("Track", format!("{track_id:?} unavailable"));
            }
        },
        None => {
            print_section_row("Track", "Nothing playing");
        }
    }

    print_section_bottom();
}

/* ============================================================
 * PROGRESS
 * ============================================================
 */

fn progress_bar(position: Duration, total: Option<Duration>, width: usize) -> String {
    let current = position.as_secs_f64();
    let total = total.map(|duration| duration.as_secs_f64()).unwrap_or(0.0);

    let filled = if total > 0.0 {
        ((current / total) * width as f64).round() as usize
    } else {
        0
    };

    let filled = filled.min(width);

    format!("{}{}", "━".repeat(filled), "·".repeat(width - filled))
}

fn print_progress(snapshot: &PlaybackSnapshot) {
    print_section_header("PROGRESS");

    let left = format_time(snapshot.position());
    let right = snapshot
        .duration()
        .map(format_time)
        .unwrap_or_else(|| "--:--".to_string());

    let bar = progress_bar(snapshot.position(), snapshot.duration(), PROGRESS_BAR_WIDTH);

    let content = format!("{left}  {bar}  {right}");

    println!("│ {:<inner$} │", content, inner = BOX_INNER);

    print_section_bottom();
}

/* ============================================================
 * QUEUE
 * ============================================================
 */

fn queue_track_label(library: &MusicLibrary, track_id: aurelis_core::library::TrackId) -> String {
    library
        .get(track_id)
        .map(|track| track.display_title().into_owned())
        .unwrap_or_else(|| format!("Unavailable {track_id:?}"))
}

fn print_queue(library: &MusicLibrary, snapshot: &PlaybackSnapshot) {
    let queue = snapshot.queue();

    print_section_header("QUEUE");

    println!(
        "│ {} │ {} │ {} │",
        pad_text_right("#", QUEUE_NO_WIDTH),
        pad_text_right("TITLE", QUEUE_TITLE_WIDTH),
        pad_text_right("ID", QUEUE_ID_WIDTH)
    );

    print_section_divider(&[QUEUE_NO_WIDTH, QUEUE_TITLE_WIDTH, QUEUE_ID_WIDTH]);

    if queue.is_empty() {
        println!(
            "│ {} │ {} │ {} │",
            " ".repeat(QUEUE_NO_WIDTH),
            pad_text_right("Queue is empty", QUEUE_TITLE_WIDTH),
            " ".repeat(QUEUE_ID_WIDTH)
        );
    } else {
        for (index, track_id) in queue.track_ids().iter().enumerate() {
            let marker = if queue.current_index() == Some(index) {
                "▶"
            } else {
                ""
            };

            let number = format!("{marker}{}", index + 1);
            let title = queue_track_label(library, *track_id);
            let id = format!("{track_id:?}");

            let number = truncate_text(&number, QUEUE_NO_WIDTH);
            let title = truncate_text(&title, QUEUE_TITLE_WIDTH);
            let id = truncate_text(&id, QUEUE_ID_WIDTH);
            println!(
                "│ {} │ {} │ {} │",
                pad_text_right(&number, QUEUE_NO_WIDTH),
                pad_text_right(&title, QUEUE_TITLE_WIDTH),
                pad_text_right(&id, QUEUE_ID_WIDTH)
            );
        }
    }

    print_section_bottom();
}

/* ============================================================
 * QUEUE POSITION
 * ============================================================
 */

fn print_queue_position(library: &MusicLibrary, snapshot: &PlaybackSnapshot) {
    let queue = snapshot.queue();

    print_section_header("QUEUE POSITION");

    let current = queue
        .current_index()
        .map(|index| format!("{} / {}", index + 1, queue.len()))
        .unwrap_or_else(|| "—".to_string());

    let previous = queue
        .previous_track_id()
        .map(|id| queue_track_label(library, id))
        .unwrap_or_else(|| "—".to_string());

    let next = queue
        .next_track_id()
        .map(|id| queue_track_label(library, id))
        .unwrap_or_else(|| "—".to_string());

    print_section_row("Current", current);
    print_section_row("Previous", previous);
    print_section_row("Next", next);

    print_section_bottom();
}

/* ============================================================
 * CONTROLS
 * ============================================================
 */

fn print_controls(queue_enabled: bool) {
    print_section_header("CONTROLS");

    let rows = if queue_enabled {
        vec![
            ("← / →", "Seek 5 seconds"),
            ("p", "Pause"),
            ("r", "Resume"),
            ("n", "Next track"),
            ("b", "Previous track"),
            ("c", "Clear queue"),
            ("status", "Technical details"),
            ("s", "Stop"),
            ("q", "Quit"),
        ]
    } else {
        vec![
            ("← / →", "Seek 5 seconds"),
            ("p", "Pause"),
            ("r", "Resume"),
            ("status", "Technical details"),
            ("s", "Stop"),
            ("q", "Quit"),
        ]
    };

    for (key, description) in rows {
        let content = format!("{key:<10} {description}");
        println!("│ {:<inner$} │", content, inner = BOX_INNER);
    }

    print_section_bottom();
}

/* ============================================================
 * TECHNICAL DETAILS
 * ============================================================
 */

fn print_track_details(library: &MusicLibrary, snapshot: &PlaybackSnapshot) {
    println!();

    let Some(track_id) = snapshot.current_track_id() else {
        print_section_header("TRACK DETAILS");
        print_section_row("Track", "Nothing selected");
        print_section_bottom();
        return;
    };

    let Some(track) = library.get(track_id) else {
        print_section_header("TRACK DETAILS");
        print_section_row("Track", format!("{track_id:?} unavailable"));
        print_section_bottom();
        return;
    };

    let metadata = track.metadata();

    print_section_header("TRACK DETAILS");
    print_section_row("Title", track.display_title());
    print_section_row("Artist", track.display_artist());
    print_section_row("Album", track.display_album());
    print_section_row("Genre", metadata.genre.as_deref().unwrap_or("Unknown"));
    print_section_row("Year", metadata.year.as_deref().unwrap_or("Unknown"));
    print_section_row(
        "Track",
        metadata
            .track
            .map(|number| number.to_string())
            .unwrap_or_else(|| "-".to_string()),
    );
    print_section_row(
        "Codec",
        if metadata.codec.is_empty() {
            "Unknown"
        } else {
            &metadata.codec
        },
    );
    print_section_row(
        "Sample Rate",
        metadata
            .sample_rate
            .map(|rate| format!("{rate} Hz"))
            .unwrap_or_else(|| "Unknown".to_string()),
    );
    print_section_row(
        "Bit Depth",
        metadata
            .bit_depth
            .map(|depth| format!("{depth}-bit"))
            .unwrap_or_else(|| "Unknown".to_string()),
    );
    print_section_row(
        "Channels",
        metadata
            .channels
            .map(|channels| channels.to_string())
            .unwrap_or_else(|| "Unknown".to_string()),
    );
    print_section_row("Duration", metadata.duration_string());
    print_section_row("Path", track.path().display());
    print_section_bottom();
}

/* ============================================================
 * COMPLETE PLAYER SCREEN
 * ============================================================
 */

fn print_player_screen(library: &MusicLibrary, snapshot: &PlaybackSnapshot) {
    print_player_header();

    println!();

    print_now_playing(library, snapshot);

    println!();

    print_progress(snapshot);

    println!();

    print_queue(library, snapshot);

    println!();

    print_queue_position(library, snapshot);

    println!();

    print_controls(true);

    println!();
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
 * LIVE PROGRESS
 * ============================================================
 */

fn clear_position_line(width: usize) -> io::Result<()> {
    if width > 0 {
        print!("\r{}\r", " ".repeat(width));
        io::stdout().flush()?;
    }

    Ok(())
}

fn finish_position_line(width: usize) -> io::Result<()> {
    if width > 0 {
        clear_position_line(width)?;
        println!();
    }

    Ok(())
}

fn format_progress_line(position: Duration, total: Option<Duration>) -> String {
    let bar = progress_bar(position, total, PROGRESS_BAR_WIDTH);

    format!(
        "  {}  {}  {}",
        format_time(position),
        bar,
        total
            .map(format_time)
            .unwrap_or_else(|| "--:--".to_string())
    )
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
    use super::{format_progress_line, truncate_text};

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
