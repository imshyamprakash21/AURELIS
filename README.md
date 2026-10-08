# AURELIS

AURELIS is a local-first Rust music player and library toolkit. The current application is a terminal player; it scans local folders, displays metadata, supports library queries, and manages playback queues.

The project is version `0.1.0` and remains under active development. Audio playback is handled by the core crate, while command routing, terminal input, playback workflows, and rendering live in the application crate.

## Build and test

Use a stable Rust toolchain that supports the Rust 2024 edition.

```powershell
cargo check --workspace
cargo test --workspace
cargo run -p aurelis-app -- --help
```

## Run

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

The core library also provides APIs for ordered playlists, local LRC/plain-text lyrics, artwork discovery and caching, and artwork-derived visual colors. Artwork is loaded lazily from embedded JPEG/PNG data first, then explicit albumart files, then common folder names such as cover, folder, front, album, and artwork; the cache retains hits and misses until cleared. During library playback, the now-playing screen and status report artwork availability, format, and source filename (or embedded source). Missing artwork uses a text fallback. The terminal reports artwork metadata without drawing image pixels.

## Known limitation

Live playback depends on a working system audio output device. On the current Windows setup, audio initialization has returned error `-2004287478`; this environment-specific device issue remains unresolved. The automated tests exercise decoding and application logic without claiming successful hardware playback.

## License

AURELIS is licensed under the MIT License. See [LICENSE](LICENSE).
