use std::{
    env,
    io::{self, BufRead, IsTerminal, Write},
    sync::mpsc,
    thread,
    time::Duration,
};

use aurelis_core::audio::{AudioMetadata, PlaybackEngine};

fn print_field(label: &str, value: Option<&str>) {
    println!("{:<12}: {}", label, value.unwrap_or("Unknown"));
}

fn print_metadata(metadata: &AudioMetadata) {
    println!("AURELIS");
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
            .unwrap_or_else(|| "-".to_string())
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
        "{:<12}: {} Hz",
        "Sample Rate",
        metadata.sample_rate.unwrap_or(0)
    );

    println!(
        "{:<12}: {}",
        "Bit Depth",
        metadata
            .bit_depth
            .map(|b| format!("{b}-bit"))
            .unwrap_or_else(|| "Unknown".to_string())
    );

    println!(
        "{:<12}: {}",
        "Channels",
        metadata
            .channels
            .map(|c| c.to_string())
            .unwrap_or_else(|| "Unknown".to_string())
    );
}

fn print_usage() {
    println!("AURELIS");
    println!("────────────────────────");
    println!();
    println!("Usage:");
    println!("  aurelis-app --help");
    println!("  aurelis-app <audio-file>");
    println!("  aurelis-app --info <audio-file>");
    println!();
    println!(
        "During playback, press ←/→ to seek 5 seconds, p to pause, r to resume, s to stop, or q to quit."
    );
}

fn format_time(position: Duration) -> String {
    format!(
        "{:02}:{:02}",
        position.as_secs() / 60,
        position.as_secs() % 60
    )
}

fn format_position(position: Duration, total: Option<Duration>) -> String {
    let current = format_time(position);

    match total {
        Some(total) => format!("{} / {}", current, format_time(total)),
        None => format!("{} / --:--", current),
    }
}

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
                0x25 => "left".to_string(),  // VK_LEFT
                0x27 => "right".to_string(), // VK_RIGHT
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

fn clear_position_line(width: usize) -> io::Result<()> {
    if width > 0 {
        print!("\r{}\r", " ".repeat(width));
        io::stdout().flush()?;
    }

    Ok(())
}

fn run_playback_controls(engine: &mut PlaybackEngine) -> Result<(), Box<dyn std::error::Error>> {
    let (command_tx, command_rx) = mpsc::channel::<String>();
    start_control_reader(command_tx)?;

    println!("Controls: ←/→ seek 5 seconds, [p]ause, [r]esume, [s]top, [q]uit");
    let total_duration = engine.metadata().duration;
    let mut last_position_second = None;
    let mut position_line_width = 0;

    loop {
        loop {
            match command_rx.try_recv() {
                Ok(command) => match command.as_str() {
                    "p" | "pause" => {
                        engine.pause();
                        let position = format_time(engine.position());
                        clear_position_line(position_line_width)?;
                        println!("Playback paused at {}.", position);
                        position_line_width = 0;
                        last_position_second = None;
                    }
                    "r" | "resume" => {
                        engine.resume();
                        let position = format_time(engine.position());
                        clear_position_line(position_line_width)?;
                        println!("Playback resumed at {}.", position);
                        position_line_width = 0;
                        last_position_second = None;
                    }
                    "left" => {
                        if let Err(error) = engine.seek_by_seconds(-5) {
                            clear_position_line(position_line_width)?;
                            println!("Seek failed: {error}");
                            position_line_width = 0;
                            last_position_second = None;
                        }
                    }
                    "right" => {
                        if let Err(error) = engine.seek_by_seconds(5) {
                            clear_position_line(position_line_width)?;
                            println!("Seek failed: {error}");
                            position_line_width = 0;
                            last_position_second = None;
                        }
                    }
                    "s" | "stop" => {
                        engine.stop();
                        let position = format_time(engine.position());
                        clear_position_line(position_line_width)?;
                        println!("Playback stopped at {}.", position);
                        return engine.wait();
                    }
                    "q" | "quit" => {
                        engine.stop();
                        let position = format_time(engine.position());
                        clear_position_line(position_line_width)?;
                        println!("Playback quit at {}.", position);
                        return engine.wait();
                    }
                    "" => {}
                    _ => {
                        clear_position_line(position_line_width)?;
                        println!("Unknown command. Use ←, →, p, r, s, or q.");
                        position_line_width = 0;
                        last_position_second = None;
                    }
                },
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }

        if let Some(result) = engine.poll() {
            println!();
            return result;
        }

        let position = engine.position();
        if last_position_second != Some(position.as_secs()) {
            let line = format_position(position, total_duration);
            print!("\r{}", line);
            io::stdout().flush()?;
            position_line_width = line.len();
            last_position_second = Some(position.as_secs());
        }

        thread::sleep(Duration::from_millis(10));
    }
}

fn main() {
    aurelis_core::initialize();

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

    /*
     * Metadata-only mode.
     *
     * Example:
     *
     * aurelis-app --info "song.flac"
     *
     * This reads metadata without opening
     * the playback engine or playing audio.
     */
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

    /*
     * Normal playback mode.
     *
     * Example:
     *
     * aurelis-app "song.flac"
     */
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
            .unwrap_or_else(|| "-".to_string())
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
        "{:<12}: {} Hz",
        "Sample Rate",
        metadata.sample_rate.unwrap_or(0)
    );

    println!(
        "{:<12}: {}",
        "Bit Depth",
        metadata
            .bit_depth
            .map(|b| format!("{b}-bit"))
            .unwrap_or_else(|| "Unknown".to_string())
    );

    println!(
        "{:<12}: {}",
        "Channels",
        metadata
            .channels
            .map(|c| c.to_string())
            .unwrap_or_else(|| "Unknown".to_string())
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

    if let Err(error) = run_playback_controls(&mut engine) {
        eprintln!("Playback failed: {error}");
    }
}
