pub mod album_art;
pub mod channel_converter;
pub mod decoder_stream;
pub mod metadata;
pub mod output;
pub mod playback;
pub mod resampler;
pub mod ring_buffer;
pub mod spectrum;
pub mod stream;
pub mod types;

pub use album_art::{AlbumArt, ArtworkFormat, ArtworkSource, extract_album_art};
pub use metadata::AudioMetadata;
pub use spectrum::{SPECTRUM_BAND_COUNT, SpectrumSnapshot};

pub use playback::{PlaybackEndReason, PlaybackEngine, PlaybackState};

pub use types::{AudioSpec, PcmBuffer};
