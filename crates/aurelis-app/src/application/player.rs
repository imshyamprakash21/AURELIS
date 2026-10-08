use std::{
    io::{self, Write},
    sync::mpsc,
    thread,
    time::Duration,
};

use aurelis_core::{
    audio::{PlaybackEndReason, PlaybackEngine},
    library::{ArtworkCache, ArtworkFormat, ArtworkSource, MusicLibrary, TrackId},
};

use crate::{
    input::start_control_reader,
    player_controller::PlaybackControlTarget,
    presentation::player::{
        ArtworkDisplay, display_width, finish_position_line, format_progress_line, format_time,
        player_terminal_width, print_controls, print_event, print_field, print_now_playing,
        print_player_screen, print_progress, print_queue, print_queue_position,
        print_track_details,
    },
};

fn artwork_display_for_track(
    library: &MusicLibrary,
    track_id: Option<TrackId>,
    cache: &mut ArtworkCache,
) -> ArtworkDisplay {
    let Some(track) = track_id.and_then(|track_id| library.get(track_id)) else {
        return ArtworkDisplay::Unavailable;
    };
    let Some(artwork) = cache.get_for_track(track) else {
        return ArtworkDisplay::Unavailable;
    };

    let format = match artwork.format() {
        ArtworkFormat::Jpeg => "JPEG",
        ArtworkFormat::Png => "PNG",
    };
    let source = match artwork.source() {
        ArtworkSource::Embedded => "embedded".to_string(),
        ArtworkSource::FileSystem { path } => format!(
            "file: {}",
            path.file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default()
        ),
    };

    ArtworkDisplay::Available { format, source }
}

pub(crate) fn run_playback_controls<T: PlaybackControlTarget>(
    engine: &mut T,
    library: Option<&MusicLibrary>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (command_tx, command_rx) = mpsc::channel::<String>();

    start_control_reader(command_tx)?;

    let mut observed_track_id = engine.current_track_id();

    let mut last_position_second = None;

    let mut position_line_width = 0;

    let mut position_line_cleared_near_end = false;
    let mut artwork_cache = ArtworkCache::new();

    /*
     * Initial screen.
     */
    if let Some(library) = library {
        if let Some(snapshot) = engine.playback_snapshot(Some(library)) {
            let artwork =
                artwork_display_for_track(library, snapshot.current_track_id(), &mut artwork_cache);
            print_player_screen(library, &snapshot, &artwork);
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
                                    let artwork = artwork_display_for_track(
                                        library,
                                        snapshot.current_track_id(),
                                        &mut artwork_cache,
                                    );
                                    print_track_details(library, &snapshot, &artwork);
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

                    let artwork = artwork_display_for_track(
                        library,
                        snapshot.current_track_id(),
                        &mut artwork_cache,
                    );
                    print_now_playing(library, &snapshot, &artwork);

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
            let line_width = display_width(&line);
            let clear_width = position_line_width
                .max(line_width)
                .min(player_terminal_width());

            print!("\r{line}{}", " ".repeat(clear_width - line_width));

            io::stdout().flush()?;

            position_line_width = clear_width;

            last_position_second = Some(position.as_secs());
        }

        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod artwork_presentation_tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use aurelis_core::{
        audio::AudioMetadata,
        library::{ArtworkCache, MusicLibrary, TrackId},
    };

    use super::artwork_display_for_track;
    use crate::presentation::player::ArtworkDisplay;

    static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "aurelis-app-artwork-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create artwork test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn test_track(directory: &Path) -> (MusicLibrary, TrackId) {
        let path = directory.join("song.flac");
        fs::write(&path, b"not an audio file").expect("write test track");
        let mut library = MusicLibrary::new();
        let id = library.add_track(path, AudioMetadata::default());
        (library, id)
    }

    fn jpeg_bytes() -> Vec<u8> {
        vec![
            0xff, 0xd8, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11,
            0x00, 0xff, 0xda, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3f, 0x00, 0x00, 0xff, 0xd9,
        ]
    }

    fn png_bytes() -> Vec<u8> {
        fn crc32(data: &[u8]) -> u32 {
            let mut crc = u32::MAX;
            for byte in data {
                crc ^= u32::from(*byte);
                for _ in 0..8 {
                    crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb8_8320 } else { 0 };
                }
            }
            !crc
        }

        fn append_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            output.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let crc_start = output.len();
            output.extend_from_slice(kind);
            output.extend_from_slice(data);
            output.extend_from_slice(&crc32(&output[crc_start..]).to_be_bytes());
        }

        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        append_chunk(
            &mut bytes,
            b"IHDR",
            &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0],
        );
        append_chunk(
            &mut bytes,
            b"IDAT",
            &[0x78, 0x9c, 0x63, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01],
        );
        append_chunk(&mut bytes, b"IEND", &[]);
        bytes
    }

    fn expected(format: &'static str, source: &str) -> ArtworkDisplay {
        ArtworkDisplay::Available {
            format,
            source: source.to_string(),
        }
    }

    #[test]
    fn exposes_current_jpeg_artwork_source_and_format() {
        let directory = TemporaryDirectory::new();
        let (library, track_id) = test_track(directory.path());
        fs::write(directory.path().join("cover.jpeg"), jpeg_bytes()).expect("write JPEG cover");
        let mut cache = ArtworkCache::new();

        let display = artwork_display_for_track(&library, Some(track_id), &mut cache);

        assert_eq!(display, expected("JPEG", "file: cover.jpeg"));
        assert_eq!(display.summary(), "Available: JPEG (file: cover.jpeg)");
    }

    #[test]
    fn exposes_current_png_artwork_source_and_format() {
        let directory = TemporaryDirectory::new();
        let (library, track_id) = test_track(directory.path());
        fs::write(directory.path().join("cover.png"), png_bytes()).expect("write PNG cover");
        let mut cache = ArtworkCache::new();

        let display = artwork_display_for_track(&library, Some(track_id), &mut cache);

        assert_eq!(display, expected("PNG", "file: cover.png"));
    }

    #[test]
    fn missing_artwork_has_a_stable_text_fallback() {
        let directory = TemporaryDirectory::new();
        let (library, track_id) = test_track(directory.path());
        let mut cache = ArtworkCache::new();

        let display = artwork_display_for_track(&library, Some(track_id), &mut cache);

        assert_eq!(display, ArtworkDisplay::Unavailable);
        assert_eq!(display.summary(), "Unavailable (text fallback)");
    }

    #[test]
    fn artwork_lookup_waits_until_a_track_is_selected() {
        let directory = TemporaryDirectory::new();
        let (library, track_id) = test_track(directory.path());
        let mut cache = ArtworkCache::new();

        assert_eq!(
            artwork_display_for_track(&library, None, &mut cache),
            ArtworkDisplay::Unavailable
        );
        fs::write(directory.path().join("cover.jpeg"), jpeg_bytes()).expect("write JPEG cover");

        assert_eq!(
            artwork_display_for_track(&library, Some(track_id), &mut cache),
            expected("JPEG", "file: cover.jpeg")
        );
    }
}

pub(crate) fn run_direct_playback(first: String) {
    let path = first;

    println!("Opening: {}\n", path);

    let mut engine = match PlaybackEngine::open(&path) {
        Ok(engine) => engine,

        Err(error) => {
            eprintln!("Failed to initialize playback for '{path}': {error}");

            return;
        }
    };
    let metadata = engine.metadata().clone();

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
