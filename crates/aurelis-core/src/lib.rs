pub mod audio;
pub mod library;

pub use library::{MusicLibrary, Track, TrackId};

pub const NAME: &str = "AURELIS";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn initialize() {
    println!("{NAME} core initialized — v{VERSION}");
}
