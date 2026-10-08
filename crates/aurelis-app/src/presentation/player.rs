use std::{
    io::{self, Write},
    time::Duration,
};

use crate::player_controller::{self, PlaybackSnapshot};
use aurelis_core::library::MusicLibrary;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const BOX_WIDTH: usize = 72;
pub(crate) const BOX_INNER: usize = BOX_WIDTH - 2;

const LABEL_WIDTH: usize = 13;
const VALUE_WIDTH: usize = BOX_INNER - LABEL_WIDTH - 6;

const QUEUE_NO_WIDTH: usize = 5;
const QUEUE_TITLE_WIDTH: usize = 42;
const QUEUE_ID_WIDTH: usize = 15;

const PROGRESS_BAR_WIDTH: usize = 40;

pub(crate) fn print_field(label: &str, value: Option<&str>) {
    println!("{:<12}  {}", label, value.unwrap_or("Unknown"));
}

pub(crate) fn format_time(position: Duration) -> String {
    format!(
        "{:02}:{:02}",
        position.as_secs() / 60,
        position.as_secs() % 60
    )
}

pub(crate) fn truncate_text(value: &str, width: usize) -> String {
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

pub(crate) fn display_width(value: &str) -> usize {
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

pub(crate) fn pad_text_right(value: &str, width: usize) -> String {
    format!(
        "{value}{}",
        " ".repeat(width.saturating_sub(display_width(value)))
    )
}

pub(crate) fn pad_text_left(value: &str, width: usize) -> String {
    format!(
        "{}{value}",
        " ".repeat(width.saturating_sub(display_width(value)))
    )
}

pub(crate) fn print_event(message: impl std::fmt::Display) {
    println!();
    println!("  ◆  {message}");
}

/* ============================================================
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

pub(crate) fn print_now_playing(library: &MusicLibrary, snapshot: &PlaybackSnapshot) {
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

pub(crate) fn print_progress(snapshot: &PlaybackSnapshot) {
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

pub(crate) fn print_queue(library: &MusicLibrary, snapshot: &PlaybackSnapshot) {
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

pub(crate) fn print_queue_position(library: &MusicLibrary, snapshot: &PlaybackSnapshot) {
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

pub(crate) fn print_controls(queue_enabled: bool) {
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

pub(crate) fn print_track_details(library: &MusicLibrary, snapshot: &PlaybackSnapshot) {
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

pub(crate) fn print_player_screen(library: &MusicLibrary, snapshot: &PlaybackSnapshot) {
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

pub(crate) fn finish_position_line(width: usize) -> io::Result<()> {
    if width > 0 {
        clear_position_line(width)?;
        println!();
    }

    Ok(())
}

pub(crate) fn format_progress_line(position: Duration, total: Option<Duration>) -> String {
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

#[cfg(test)]
mod tests {
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
