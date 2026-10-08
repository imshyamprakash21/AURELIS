mod application;
mod cli;
mod input;
mod library_browser;
mod library_play_test;
mod library_search;
mod library_test;
mod player_controller;
mod presentation;

pub use application::desktop::{
    AlbumSummary, ArtistSummary, ArtworkImageFormat, ArtworkView, DesktopApplication,
    DesktopPlaybackState, FolderSummary, GenreSummary, LibraryScanSummary, LibrarySearchResults,
    PlaybackView, TrackSummary,
};
pub use aurelis_core::library::{ArtworkVisual, Lyrics, SyncedLyricLine, TrackId};

/// Run the existing command-line interface.
pub fn run_cli() {
    aurelis_core::initialize();
    cli::run();
}
