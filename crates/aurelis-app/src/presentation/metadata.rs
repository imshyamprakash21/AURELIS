use aurelis_core::audio::AudioMetadata;

use crate::presentation::player::print_field;

pub(crate) fn print_metadata(metadata: &AudioMetadata) {
    println!("AURELIS");
    println!("────────");
    println!();

    print_field("Title", metadata.title.as_deref());
    print_field("Artist", metadata.artist.as_deref());
    print_field("Album", metadata.album.as_deref());
    print_field("Genre", metadata.genre.as_deref());
    print_field("Year", metadata.year.as_deref());

    println!(
        "{:<12}  {}",
        "Track",
        metadata
            .track
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".to_string())
    );

    println!("{:<12}  {}", "Duration", metadata.duration_string());

    println!(
        "{:<12}  {}",
        "Codec",
        if metadata.codec.is_empty() {
            "Unknown"
        } else {
            &metadata.codec
        }
    );

    println!(
        "{:<12}  {}",
        "Sample Rate",
        metadata
            .sample_rate
            .map(|rate| format!("{rate} Hz"))
            .unwrap_or_else(|| "Unknown".to_string())
    );

    println!(
        "{:<12}  {}",
        "Bit Depth",
        metadata
            .bit_depth
            .map(|b| format!("{b}-bit"))
            .unwrap_or_else(|| "Unknown".to_string())
    );

    println!(
        "{:<12}  {}",
        "Channels",
        metadata
            .channels
            .map(|c| c.to_string())
            .unwrap_or_else(|| "Unknown".to_string())
    );
}
