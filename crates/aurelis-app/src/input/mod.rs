use std::{
    io::{self, BufRead, IsTerminal},
    sync::mpsc,
    thread,
};

pub(crate) fn start_control_reader(command_tx: mpsc::Sender<String>) -> io::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::normalize_control_line;

    #[test]
    fn ignores_empty_control_lines() {
        assert_eq!(normalize_control_line(""), None);
        assert_eq!(normalize_control_line("  	 "), None);
    }

    #[test]
    fn normalizes_left_and_right_arrow_sequences() {
        for line in ["\u{1b}[D", "\u{1b}OD", "  LEFT  "] {
            assert_eq!(normalize_control_line(line).as_deref(), Some("left"));
        }
        for line in ["\u{1b}[C", "\u{1b}OC", "  RIGHT  "] {
            assert_eq!(normalize_control_line(line).as_deref(), Some("right"));
        }
    }

    #[test]
    fn trims_and_lowercases_named_commands() {
        assert_eq!(normalize_control_line("  NEXT  ").as_deref(), Some("next"));
        assert_eq!(normalize_control_line(" Q ").as_deref(), Some("q"));
    }
}
