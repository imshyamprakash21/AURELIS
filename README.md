# AURELIS

AURELIS is a local-first Rust music player and library toolkit. AURELIS includes a native desktop player and a terminal diagnostic interface. Both use the same Rust audio engine, library scanner, artwork cache, and application playback controller.

The project is version `0.1.0` and remains under active development. The core crate handles audio playback and the artwork visual model. The application crate provides library and playback workflows, command routing, terminal input, and shared presentation helpers; the desktop crate provides the native window, UI, and nonblocking artwork and lyric presentation.

## Build and test

Use a stable Rust toolchain that supports the Rust 2024 edition.

```powershell
cargo check --workspace
cargo test --workspace
cargo run -p aurelis-app -- --help
```

## Run

Launch the native desktop application with `cargo run -p aurelis-desktop`.
Use `cargo run -p aurelis-app -- --help` for the command-line diagnostic interface.

```powershell
# Play a file
cargo run -p aurelis-app -- "C:\Music\song.flac"

# Browse a local library
cargo run -p aurelis-app -- library "C:\Music"

# Search the library
cargo run -p aurelis-app -- library-search "C:\Music" "Blue Train"

# Filter and sort a library query
cargo run -p aurelis-app -- library-query "C:\Music" --artist "John Coltrane" --sort album --order asc

# Play every discovered track in order
cargo run -p aurelis-app -- library-play "C:\Music" --all

# Display technical metadata
cargo run -p aurelis-app -- --info "C:\Music\song.flac"
```

Run `aurelis-app --help` for the complete command list. Library scanning currently recognizes FLAC, MP3, AAC, M4A, OGG, and WAV file extensions. Direct playback uses the file decoders provided by Symphonia.

During playback, left/right arrows seek by five seconds; `p` pauses, `r` resumes, `s` stops, and `q` quits. Library playback also supports `n`/`b` for next/previous track and `c` to clear the remaining queue. The `status` command shows technical details and artwork availability/source during library playback.

## Library data

The library index is stored under `.aurelis_cache/library-index.json` inside each scanned music folder. It is rebuilt when missing or invalid and is ignored by Git. AURELIS does not require network access or a database for local playback and scanning.

The core library also provides APIs for ordered playlists, local LRC/plain-text lyrics, artwork discovery and caching, and artwork-derived visual colors. The desktop Now Playing screen loads same-stem `.lrc` or `.txt` sidecars off the UI thread, keeps a bounded cache for the current library, follows synchronized lines using the real playback position, and scrolls the current line into view; plain text remains scrollable in its original language. Artwork is loaded lazily from embedded JPEG/PNG data first, then explicit albumart files, then common folder names such as cover, folder, front, album, and artwork; the cache retains hits and misses until cleared. The desktop presentation decodes artwork off the UI thread and passes a bounded pixel sample to the core visual model, caching each track's derived visual for the current library. Now Playing uses the resulting dominant and accent colors for a subtle background tint and artist accent, transitioning over 200 ms when artwork changes. Missing or invalid artwork returns to the fixed AURELIS theme. The screen reports artwork availability, format, and source filename (or embedded source); the terminal reports artwork metadata without drawing image pixels.

## Known limitation

Live playback depends on an available system audio output device and working drivers. Hardware output should be verified on the target machine; automated tests cover decoding and application logic without claiming successful hardware playback.

Desktop lyrics currently load same-stem `.lrc` files with line timestamps and `.txt` files as plain text. Embedded lyrics and enhanced word-timed `.elrc` display are not implemented.

## License

AURELIS is licensed under the MIT License. See [LICENSE](LICENSE).
