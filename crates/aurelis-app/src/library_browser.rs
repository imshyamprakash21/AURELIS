//! CLI rendering for the read-only music library browser.

use std::{fmt::Write as _, io::ErrorKind, path::Path};

use aurelis_core::library::{DiscoveryIssue, MusicLibrary, ScanIssue, ScanReport};

use super::{BOX_INNER, display_width, pad_text_left, pad_text_right, truncate_text};

const ID_WIDTH: usize = 12;
const TITLE_WIDTH: usize = 22;
const ARTIST_WIDTH: usize = 17;
const DURATION_WIDTH: usize = 8;
const COLUMN_WIDTHS: [usize; 4] = [ID_WIDTH, TITLE_WIDTH, ARTIST_WIDTH, DURATION_WIDTH];

pub(super) fn run(root: &str) {
    let root_path = Path::new(root);
    let mut library = MusicLibrary::new();
    let report = library.scan_directory(root_path);

    let output = match root_scan_issue(&report, root_path) {
        Some(issue) => render_root_error(root_path, issue),
        None => render_library(&library, &report),
    };

    print!("{output}");
}

fn root_scan_issue<'a>(report: &'a ScanReport, root: &Path) -> Option<&'a DiscoveryIssue> {
    report.issues.iter().find_map(|issue| match issue {
        ScanIssue::Discovery(issue) if issue.path == root => Some(issue),
        _ => None,
    })
}

fn render_root_error(root: &Path, issue: &DiscoveryIssue) -> String {
    let message = match issue.kind {
        ErrorKind::NotFound => "The selected directory could not be found.",
        ErrorKind::PermissionDenied => "Access to the selected directory was denied.",
        ErrorKind::InvalidInput if issue.message.contains("not a directory") => {
            "The selected path is not a directory."
        }
        ErrorKind::InvalidInput if issue.message.contains("symbolic-link") => {
            "Symbolic-link directories are not followed."
        }
        _ => "The selected directory could not be read.",
    };

    let mut output = String::new();
    writeln!(&mut output).expect("writing to a String cannot fail");
    push_library_heading(&mut output);
    push_full_width_border(&mut output, '├', '┤');
    push_message_row(&mut output, message);
    push_full_width_border(&mut output, '╰', '╯');
    writeln!(&mut output).expect("writing to a String cannot fail");
    writeln!(&mut output, "  Path: {}", root.display()).expect("writing to a String cannot fail");
    output
}

fn render_library(library: &MusicLibrary, report: &ScanReport) -> String {
    let mut output = String::new();
    writeln!(&mut output).expect("writing to a String cannot fail");
    push_library_heading(&mut output);

    if library.is_empty() {
        push_full_width_border(&mut output, '├', '┤');
        let message = if report.discovered == 0 && report.issues.is_empty() {
            "No supported audio files were found."
        } else if report.failed > 0 {
            "No tracks with readable metadata were found."
        } else {
            "No readable audio tracks could be found."
        };
        push_message_row(&mut output, message);
        push_full_width_border(&mut output, '╰', '╯');
    } else {
        push_table_border(&mut output, &COLUMN_WIDTHS, '├', '┬', '┤');
        push_library_row(&mut output, "TRACK ID", "TITLE", "ARTIST", "DURATION");
        push_table_border(&mut output, &COLUMN_WIDTHS, '├', '┼', '┤');

        for track in library.iter() {
            push_library_row(
                &mut output,
                &format!("{:?}", track.id()),
                &track.display_title(),
                &track.display_artist(),
                &track.metadata().duration_string(),
            );
        }

        push_table_border(&mut output, &COLUMN_WIDTHS, '╰', '┴', '╯');
    }

    writeln!(&mut output).expect("writing to a String cannot fail");
    write!(
        &mut output,
        "  {}  ·  {} added  ·  {} updated  ·  {} failed",
        track_count(library.len()),
        report.added,
        report.updated,
        report.failed
    )
    .expect("writing to a String cannot fail");
    if !report.issues.is_empty() {
        write!(
            &mut output,
            "  ·  {} scan issue{}",
            report.issues.len(),
            if report.issues.len() == 1 { "" } else { "s" }
        )
        .expect("writing to a String cannot fail");
    }
    output.push('\n');

    output
}

fn track_count(count: usize) -> String {
    format!("{count} {}", if count == 1 { "track" } else { "tracks" })
}

fn push_library_heading(output: &mut String) {
    writeln!(output, "╭{}╮", "─".repeat(BOX_INNER)).expect("writing to a String cannot fail");
    let title = "YOUR LIBRARY";
    let remaining = BOX_INNER.saturating_sub(display_width(title));
    let left_padding = remaining / 2;
    let right_padding = remaining - left_padding;
    writeln!(
        output,
        "│{}{}{}│",
        " ".repeat(left_padding),
        title,
        " ".repeat(right_padding)
    )
    .expect("writing to a String cannot fail");
}

fn push_full_width_border(output: &mut String, left: char, right: char) {
    writeln!(output, "{left}{}{right}", "─".repeat(BOX_INNER))
        .expect("writing to a String cannot fail");
}

fn push_message_row(output: &mut String, message: &str) {
    let content_width = BOX_INNER - 2;
    let message = truncate_text(message, content_width);
    writeln!(output, "│ {} │", pad_text_right(&message, content_width))
        .expect("writing to a String cannot fail");
}

pub(super) fn push_table_border(
    output: &mut String,
    widths: &[usize],
    left: char,
    junction: char,
    right: char,
) {
    output.push(left);
    for (index, width) in widths.iter().enumerate() {
        if index > 0 {
            output.push(junction);
        }
        output.push_str(&"─".repeat(width + 2));
    }
    output.push(right);
    output.push('\n');
}
fn push_library_row(output: &mut String, id: &str, title: &str, artist: &str, duration: &str) {
    let id = truncate_text(id, ID_WIDTH);
    let title = truncate_text(title, TITLE_WIDTH);
    let artist = truncate_text(artist, ARTIST_WIDTH);
    let duration = truncate_text(duration, DURATION_WIDTH);

    writeln!(
        output,
        "│ {} │ {} │ {} │ {} │",
        pad_text_right(&id, ID_WIDTH),
        pad_text_right(&title, TITLE_WIDTH),
        pad_text_right(&artist, ARTIST_WIDTH),
        pad_text_left(&duration, DURATION_WIDTH)
    )
    .expect("writing to a String cannot fail");
}

#[cfg(test)]
mod tests {
    use super::*;
    use aurelis_core::audio::AudioMetadata;
    use std::{path::PathBuf, time::Duration};
    use unicode_width::UnicodeWidthStr;

    const BOX_WIDTH: usize = BOX_INNER + 2;

    fn assert_box_alignment(rendered: &str) {
        for line in rendered.lines().filter(|line| {
            line.starts_with('╭')
                || line.starts_with('│')
                || line.starts_with('├')
                || line.starts_with('╰')
        }) {
            assert_eq!(UnicodeWidthStr::width(line), BOX_WIDTH, "{line:?}");
        }
    }

    #[test]
    fn renders_a_populated_library_with_browsing_columns_only() {
        let mut library = MusicLibrary::new();
        library.add_track(
            PathBuf::from("Music/Artist/Album/song.flac"),
            AudioMetadata {
                title: Some("Song title".to_owned()),
                artist: Some("Example artist".to_owned()),
                duration: Some(Duration::from_secs(200)),
                codec: "FLAC".to_owned(),
                ..AudioMetadata::default()
            },
        );
        let report = ScanReport {
            discovered: 1,
            added: 1,
            ..ScanReport::default()
        };

        let rendered = render_library(&library, &report);

        assert!(rendered.contains("YOUR LIBRARY"));
        assert!(rendered.contains("TrackId(1)"));
        assert!(rendered.contains("Song title"));
        assert!(rendered.contains("Example artist"));
        assert!(rendered.contains("03:20"));
        assert!(rendered.contains("1 track  ·  1 added  ·  0 updated  ·  0 failed"));
        assert!(!rendered.contains("FLAC"));
        assert!(!rendered.contains("Music/Artist/Album/song.flac"));
        assert_box_alignment(&rendered);
    }

    #[test]
    fn renders_empty_library_without_table_noise() {
        let library = MusicLibrary::new();
        let report = ScanReport::default();

        let rendered = render_library(&library, &report);

        assert!(rendered.contains("No supported audio files were found."));
        assert!(rendered.contains("0 tracks  ·  0 added  ·  0 updated  ·  0 failed"));
        assert!(!rendered.contains("TRACK ID"));
        assert_box_alignment(&rendered);
    }

    #[test]
    fn truncates_long_unicode_cells_to_keep_every_table_border_aligned() {
        let mut library = MusicLibrary::new();
        library.add_track(
            "Music/Artist/Album/song.flac",
            AudioMetadata {
                title: Some("界🙂界🙂界🙂界🙂界🙂界🙂界🙂界🙂".to_owned()),
                artist: Some("東京の音楽家東京の音楽家東京の音楽家".to_owned()),
                ..AudioMetadata::default()
            },
        );
        let report = ScanReport {
            discovered: 1,
            added: 1,
            ..ScanReport::default()
        };

        let rendered = render_library(&library, &report);

        assert!(rendered.contains("..."));
        assert_box_alignment(&rendered);
    }

    #[test]
    fn missing_metadata_uses_track_display_fallbacks_and_unknown_duration() {
        let mut library = MusicLibrary::new();
        library.add_track(
            "Music/Artist/Album/fallback-title.flac",
            AudioMetadata::default(),
        );
        let report = ScanReport {
            discovered: 1,
            added: 1,
            ..ScanReport::default()
        };

        let rendered = render_library(&library, &report);

        assert!(rendered.contains("fallback-title"));
        assert!(rendered.contains("Artist"));
        assert!(rendered.contains("--:--"));
        assert_box_alignment(&rendered);
    }

    #[test]
    fn scan_failure_message_is_plain_language() {
        let issue = DiscoveryIssue {
            path: PathBuf::from("missing"),
            kind: ErrorKind::NotFound,
            message: "OS error 2".to_owned(),
        };

        let rendered = render_root_error(Path::new("missing"), &issue);

        assert!(rendered.contains("The selected directory could not be found."));
        assert!(!rendered.contains("NotFound"));
        assert!(!rendered.contains("OS error"));
        assert_box_alignment(&rendered);
    }
}
