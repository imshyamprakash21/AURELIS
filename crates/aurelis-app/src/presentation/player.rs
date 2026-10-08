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
const VALUE_WIDTH: usize = BOX_WIDTH - LABEL_WIDTH - 6;

const QUEUE_NO_WIDTH: usize = 5;
const QUEUE_TITLE_WIDTH: usize = 42;
const QUEUE_ID_WIDTH: usize = 15;

const PROGRESS_BAR_WIDTH: usize = 40;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ArtworkDisplay {
    Available {
        format: &'static str,
        source: String,
    },
    Unavailable,
}

impl ArtworkDisplay {
    pub(crate) fn summary(&self) -> String {
        match self {
            Self::Available { format, source } => format!("Available: {format} ({source})"),
            Self::Unavailable => "Unavailable (text fallback)".to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PlayerLayout {
    width: usize,
    inner_width: usize,
    row_content_width: usize,
    label_width: usize,
    value_width: usize,
    queue_no_width: usize,
    queue_title_width: usize,
    queue_id_width: usize,
    progress_bar_width: usize,
}

impl PlayerLayout {
    fn for_width(width: usize) -> Self {
        let width = width.clamp(10, BOX_WIDTH);
        let queue_column_space = width.saturating_sub(10);
        let queue_no_width = QUEUE_NO_WIDTH.min(queue_column_space.saturating_sub(1));
        let queue_id_width =
            QUEUE_ID_WIDTH.min(queue_column_space.saturating_sub(queue_no_width + 1));
        let queue_title_width = if width == BOX_WIDTH {
            QUEUE_TITLE_WIDTH
        } else {
            queue_column_space.saturating_sub(queue_no_width + queue_id_width)
        };
        let label_width = LABEL_WIDTH.min(width.saturating_sub(7));

        Self {
            width,
            inner_width: width.saturating_sub(2),
            row_content_width: width.saturating_sub(4),
            label_width,
            value_width: if width == BOX_WIDTH {
                VALUE_WIDTH
            } else {
                width.saturating_sub(label_width + 6)
            },
            queue_no_width,
            queue_title_width,
            queue_id_width,
            progress_bar_width: PROGRESS_BAR_WIDTH.min(width.saturating_sub(18)),
        }
    }
}

fn current_layout() -> PlayerLayout {
    PlayerLayout::for_width(terminal_columns())
}

pub(crate) fn player_terminal_width() -> usize {
    current_layout().width
}

fn terminal_columns() -> usize {
    #[cfg(windows)]
    if let Some(width) = windows_console_columns() {
        return width;
    }

    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|width| *width > 0)
        .unwrap_or(BOX_WIDTH)
}

#[cfg(windows)]
fn windows_console_columns() -> Option<usize> {
    use windows::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetStdHandle, STD_OUTPUT_HANDLE,
    };

    let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }.ok()?;
    let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
    unsafe { GetConsoleScreenBufferInfo(output, &mut info) }.ok()?;

    let width = i32::from(info.srWindow.Right) - i32::from(info.srWindow.Left) + 1;
    usize::try_from(width).ok().filter(|width| *width > 0)
}

fn format_section_row(label: &str, value: &str, layout: PlayerLayout) -> String {
    let label = truncate_text(label, layout.label_width);
    let value = truncate_text(value, layout.value_width);
    format!(
        "│ {}│ {} │",
        pad_text_right(&label, layout.label_width),
        pad_text_right(&value, layout.value_width)
    )
}

fn format_content_row(content: &str, layout: PlayerLayout) -> String {
    let content = truncate_text(content, layout.row_content_width);
    format!("│ {} │", pad_text_right(&content, layout.row_content_width))
}

fn format_queue_row(number: &str, title: &str, id: &str, layout: PlayerLayout) -> String {
    let number = truncate_text(number, layout.queue_no_width);
    let title = truncate_text(title, layout.queue_title_width);
    let id = truncate_text(id, layout.queue_id_width);
    format!(
        "│ {} │ {} │ {} │",
        pad_text_right(&number, layout.queue_no_width),
        pad_text_right(&title, layout.queue_title_width),
        pad_text_right(&id, layout.queue_id_width)
    )
}

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
    let layout = current_layout();
    println!();
    println!(
        "{}",
        truncate_text(&format!("  ◆  {message}"), layout.width)
    );
}

/* ============================================================
 * BOX PRIMITIVES
 * ============================================================
 */

fn print_player_header(layout: PlayerLayout) {
    let title = truncate_text("AURELIS  PLAYER", layout.inner_width);
    let title_width = display_width(&title);
    let left_padding = layout.inner_width.saturating_sub(title_width) / 2;
    let right_padding = layout
        .inner_width
        .saturating_sub(title_width + left_padding);

    println!();
    println!("╭{}╮", "─".repeat(layout.inner_width));
    println!(
        "│{}{}{}│",
        " ".repeat(left_padding),
        title,
        " ".repeat(right_padding)
    );
    println!("╰{}╯", "─".repeat(layout.inner_width));
}

fn print_section_header(title: &str, layout: PlayerLayout) {
    let title = truncate_text(title, layout.inner_width.saturating_sub(4));
    let title = format!("  {title}  ");
    let remaining = layout.inner_width.saturating_sub(display_width(&title));

    println!("┌{}{}┐", title, "─".repeat(remaining));
}

fn print_section_bottom(layout: PlayerLayout) {
    println!("└{}┘", "─".repeat(layout.inner_width));
}

fn print_section_row(label: &str, value: impl std::fmt::Display, layout: PlayerLayout) {
    println!("{}", format_section_row(label, &value.to_string(), layout));
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

pub(crate) fn print_now_playing(
    library: &MusicLibrary,
    snapshot: &PlaybackSnapshot,
    artwork: &ArtworkDisplay,
) {
    let layout = current_layout();
    print_section_header("NOW PLAYING", layout);

    match snapshot.current_track_id() {
        Some(track_id) => match library.get(track_id) {
            Some(track) => {
                print_section_row("Title", track.display_title(), layout);
                print_section_row("Artist", track.display_artist(), layout);
                print_section_row("Album", track.display_album(), layout);
                print_section_row("State", playback_state_label(snapshot), layout);
            }
            None => {
                print_section_row("Track", format!("{track_id:?} unavailable"), layout);
            }
        },
        None => {
            print_section_row("Track", "Nothing playing", layout);
        }
    }

    print_section_row("Artwork", artwork.summary(), layout);
    print_section_bottom(layout);
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
    let layout = current_layout();
    print_section_header("PROGRESS", layout);

    let left = format_time(snapshot.position());
    let right = snapshot
        .duration()
        .map(format_time)
        .unwrap_or_else(|| "--:--".to_string());

    let bar = progress_bar(
        snapshot.position(),
        snapshot.duration(),
        layout.progress_bar_width,
    );

    let content = format!("{left}  {bar}  {right}");

    println!("{}", format_content_row(&content, layout));

    print_section_bottom(layout);
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
    let layout = current_layout();

    print_section_header("QUEUE", layout);

    println!("{}", format_queue_row("#", "TITLE", "ID", layout));

    print_section_divider(&[
        layout.queue_no_width,
        layout.queue_title_width,
        layout.queue_id_width,
    ]);

    if queue.is_empty() {
        println!("{}", format_queue_row("", "Queue is empty", "", layout));
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

            println!("{}", format_queue_row(&number, &title, &id, layout));
        }
    }

    print_section_bottom(layout);
}

/* ============================================================
 * QUEUE POSITION
 * ============================================================
 */

pub(crate) fn print_queue_position(library: &MusicLibrary, snapshot: &PlaybackSnapshot) {
    let queue = snapshot.queue();
    let layout = current_layout();

    print_section_header("QUEUE POSITION", layout);

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

    print_section_row("Current", current, layout);
    print_section_row("Previous", previous, layout);
    print_section_row("Next", next, layout);

    print_section_bottom(layout);
}

/* ============================================================
 * CONTROLS
 * ============================================================
 */

pub(crate) fn print_controls(queue_enabled: bool) {
    let layout = current_layout();
    print_section_header("CONTROLS", layout);

    let rows = if queue_enabled {
        vec![
            ("← / →", "Seek 5 seconds"),
            ("p", "Pause"),
            ("r", "Resume"),
            ("n", "Next track"),
            ("b", "Previous track"),
            ("c", "Clear queue"),
            ("status", "Track and artwork details"),
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
        println!("{}", format_content_row(&content, layout));
    }

    print_section_bottom(layout);
}

/* ============================================================
 * TECHNICAL DETAILS
 * ============================================================
 */

pub(crate) fn print_track_details(
    library: &MusicLibrary,
    snapshot: &PlaybackSnapshot,
    artwork: &ArtworkDisplay,
) {
    println!();
    let layout = current_layout();

    let Some(track_id) = snapshot.current_track_id() else {
        print_section_header("TRACK DETAILS", layout);
        print_section_row("Track", "Nothing selected", layout);
        print_section_row("Artwork", artwork.summary(), layout);
        print_section_bottom(layout);
        return;
    };

    let Some(track) = library.get(track_id) else {
        print_section_header("TRACK DETAILS", layout);
        print_section_row("Track", format!("{track_id:?} unavailable"), layout);
        print_section_row("Artwork", artwork.summary(), layout);
        print_section_bottom(layout);
        return;
    };

    let metadata = track.metadata();

    print_section_header("TRACK DETAILS", layout);
    print_section_row("Title", track.display_title(), layout);
    print_section_row("Artist", track.display_artist(), layout);
    print_section_row("Album", track.display_album(), layout);
    print_section_row(
        "Genre",
        metadata.genre.as_deref().unwrap_or("Unknown"),
        layout,
    );
    print_section_row(
        "Year",
        metadata.year.as_deref().unwrap_or("Unknown"),
        layout,
    );
    print_section_row(
        "Track",
        metadata
            .track
            .map(|number| number.to_string())
            .unwrap_or_else(|| "-".to_string()),
        layout,
    );
    print_section_row(
        "Codec",
        if metadata.codec.is_empty() {
            "Unknown"
        } else {
            &metadata.codec
        },
        layout,
    );
    print_section_row(
        "Sample Rate",
        metadata
            .sample_rate
            .map(|rate| format!("{rate} Hz"))
            .unwrap_or_else(|| "Unknown".to_string()),
        layout,
    );
    print_section_row(
        "Bit Depth",
        metadata
            .bit_depth
            .map(|depth| format!("{depth}-bit"))
            .unwrap_or_else(|| "Unknown".to_string()),
        layout,
    );
    print_section_row(
        "Channels",
        metadata
            .channels
            .map(|channels| channels.to_string())
            .unwrap_or_else(|| "Unknown".to_string()),
        layout,
    );
    print_section_row("Duration", metadata.duration_string(), layout);
    print_section_row("Path", track.path().display(), layout);
    print_section_row("Artwork", artwork.summary(), layout);
    print_section_bottom(layout);
}

/* ============================================================
 * COMPLETE PLAYER SCREEN
 * ============================================================
 */

pub(crate) fn print_player_screen(
    library: &MusicLibrary,
    snapshot: &PlaybackSnapshot,
    artwork: &ArtworkDisplay,
) {
    print_player_header(current_layout());

    println!();

    print_now_playing(library, snapshot, artwork);

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
    format_progress_line_with_layout(position, total, current_layout())
}

fn format_progress_line_with_layout(
    position: Duration,
    total: Option<Duration>,
    layout: PlayerLayout,
) -> String {
    let bar = progress_bar(position, total, layout.progress_bar_width);

    let line = format!(
        "  {}  {}  {}",
        format_time(position),
        bar,
        total
            .map(format_time)
            .unwrap_or_else(|| "--:--".to_string())
    );
    truncate_text(&line, layout.width)
}

#[cfg(test)]
mod tests {
    use super::{
        BOX_INNER, BOX_WIDTH, LABEL_WIDTH, PROGRESS_BAR_WIDTH, PlayerLayout, QUEUE_ID_WIDTH,
        QUEUE_NO_WIDTH, QUEUE_TITLE_WIDTH, VALUE_WIDTH, display_width, format_content_row,
        format_progress_line, format_progress_line_with_layout, format_queue_row,
        format_section_row, truncate_text,
    };

    use std::time::Duration;

    #[test]
    fn default_layout_keeps_the_player_design_width_and_fits_each_row() {
        let layout = PlayerLayout::for_width(BOX_WIDTH);

        assert_eq!(layout.width, BOX_WIDTH);
        assert_eq!(layout.inner_width, BOX_INNER);
        assert_eq!(layout.label_width, LABEL_WIDTH);
        assert_eq!(layout.value_width, VALUE_WIDTH);
        assert_eq!(layout.queue_no_width, QUEUE_NO_WIDTH);
        assert_eq!(layout.queue_title_width, QUEUE_TITLE_WIDTH);
        assert_eq!(layout.queue_id_width, QUEUE_ID_WIDTH);
        assert_eq!(layout.progress_bar_width, PROGRESS_BAR_WIDTH);
        assert_eq!(
            display_width(&format_section_row("Artist", "Name", layout)),
            72
        );
        assert_eq!(display_width(&format_content_row("Controls", layout)), 72);
        assert_eq!(
            display_width(&format_queue_row("1", "Song", "TrackId(1)", layout)),
            72
        );
    }

    #[test]
    fn player_rows_and_progress_fit_a_narrow_terminal_with_unicode() {
        let layout = PlayerLayout::for_width(48);
        let rows = [
            format_section_row("Artist", "音楽とvery long artist name", layout),
            format_content_row("← / →  Seek five seconds and continue", layout),
            format_queue_row(
                "▶ 123",
                "日本語の長いタイトル song",
                "TrackId(123456)",
                layout,
            ),
        ];

        assert!(rows.iter().all(|row| display_width(row) == 48));
        let progress = format_progress_line_with_layout(
            Duration::from_secs(30),
            Some(Duration::from_secs(90)),
            layout,
        );
        assert!(display_width(&progress) <= 48);
    }

    #[test]
    fn player_layout_caps_to_the_existing_preferred_width() {
        assert_eq!(PlayerLayout::for_width(120).width, BOX_WIDTH);
    }

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
