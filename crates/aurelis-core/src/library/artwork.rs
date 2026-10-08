use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use symphonia::core::{io::MediaSourceStream, meta::Visual, probe::Hint};
use symphonia::default::get_probe;

use super::Track;

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const EXTERNAL_ARTWORK_NAMES: &[&str] = &[
    "cover.jpg",
    "cover.jpeg",
    "cover.png",
    "folder.jpg",
    "folder.jpeg",
    "folder.png",
    "front.jpg",
    "front.jpeg",
    "front.png",
    "album.jpg",
    "album.jpeg",
    "album.png",
    "albumart.jpg",
    "albumart.jpeg",
    "albumart.png",
    "albumartsmall.jpg",
    "albumartsmall.jpeg",
    "albumartsmall.png",
];

/// The encoded format of supported artwork data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtworkFormat {
    /// JPEG image data.
    Jpeg,
    /// PNG image data.
    Png,
}

impl ArtworkFormat {
    /// MIME type of the encoded image.
    pub const fn mime_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
        }
    }

    /// Conventional file extension of the encoded image.
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
        }
    }
}

/// Raw JPEG or PNG artwork associated with a library track.
///
/// Pixel decoding is intentionally left to a presentation consumer.
#[derive(Debug, Eq, PartialEq)]
pub struct Artwork {
    data: Box<[u8]>,
    format: ArtworkFormat,
}

impl Artwork {
    /// Encoded image bytes.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Encoded image format.
    pub const fn format(&self) -> ArtworkFormat {
        self.format
    }

    /// MIME type of the encoded image.
    pub const fn mime_type(&self) -> &'static str {
        self.format.mime_type()
    }

    /// File extension commonly used for the encoded image.
    pub const fn extension(&self) -> &'static str {
        self.format.extension()
    }

    fn from_bytes(data: Vec<u8>) -> Option<Self> {
        let format = detect_format(&data)?;
        Some(Self {
            data: data.into_boxed_slice(),
            format,
        })
    }

    fn from_visual(visual: &Visual) -> Option<Self> {
        Self::from_bytes(visual.data.to_vec())
    }
}

/// Loads artwork on demand and shares cached images between tracks.
///
/// Both successful lookups and missing artwork are cached. Call [`ArtworkCache::clear`]
/// when the library's files may have changed and lookups should be refreshed.
#[derive(Debug, Default)]
pub struct ArtworkCache {
    by_track: HashMap<PathBuf, Option<Arc<Artwork>>>,
    by_directory: HashMap<PathBuf, Vec<PathBuf>>,
    by_external_file: HashMap<PathBuf, Option<Arc<Artwork>>>,
}

impl ArtworkCache {
    /// Create an empty artwork cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get artwork for a track, loading it only when first requested.
    pub fn get_for_track(&mut self, track: &Track) -> Option<Arc<Artwork>> {
        let track_path = track.path().to_path_buf();
        if let Some(artwork) = self.by_track.get(&track_path) {
            return artwork.clone();
        }

        let mut artwork = None;
        for image_path in self.external_artwork_paths(&track_path) {
            if let Some(found) = self.external_artwork(&image_path) {
                artwork = Some(found);
                break;
            }
        }

        if artwork.is_none() {
            artwork = extract_embedded_artwork(&track_path).map(Arc::new);
        }

        self.by_track.insert(track_path, artwork.clone());
        artwork
    }

    /// Forget cached artwork and cached misses.
    pub fn clear(&mut self) {
        self.by_track.clear();
        self.by_directory.clear();
        self.by_external_file.clear();
    }

    fn external_artwork_paths(&mut self, track_path: &Path) -> Vec<PathBuf> {
        let Some(directory) = track_path.parent() else {
            return Vec::new();
        };
        if let Some(paths) = self.by_directory.get(directory) {
            return paths.clone();
        }

        let paths = external_artwork_paths(track_path);
        self.by_directory
            .insert(directory.to_path_buf(), paths.clone());
        paths
    }

    fn external_artwork(&mut self, path: &Path) -> Option<Arc<Artwork>> {
        let key = path.to_path_buf();
        if let Some(artwork) = self.by_external_file.get(&key) {
            return artwork.clone();
        }

        let artwork = read_external_artwork(path).map(Arc::new);
        self.by_external_file.insert(key, artwork.clone());
        artwork
    }
}

/// Load artwork for a track without retaining a cache.
///
/// Common images in the track's directory are preferred. If none is readable,
/// the first supported embedded image is returned. File, probe, and image
/// validation failures are treated as missing artwork so they do not prevent
/// library use or playback.
pub fn load_artwork_for_track(track: &Track) -> Option<Artwork> {
    for path in external_artwork_paths(track.path()) {
        if let Some(artwork) = read_external_artwork(&path) {
            return Some(artwork);
        }
    }

    extract_embedded_artwork(track.path())
}

fn external_artwork_paths(track_path: &Path) -> Vec<PathBuf> {
    let Some(directory) = track_path.parent() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };

    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|file_type| file_type.is_file())
                .map(|_| entry.path())
        })
        .collect();
    files.sort_by_key(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default()
    });

    EXTERNAL_ARTWORK_NAMES
        .iter()
        .filter_map(|candidate| {
            files
                .iter()
                .find(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.eq_ignore_ascii_case(candidate))
                })
                .cloned()
        })
        .collect()
}

fn read_external_artwork(path: &Path) -> Option<Artwork> {
    Artwork::from_bytes(fs::read(path).ok()?)
}

fn extract_embedded_artwork(path: &Path) -> Option<Artwork> {
    let file = fs::File::open(path).ok()?;
    let media_source = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();

    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        hint.with_extension(extension);
    }

    let probed = get_probe()
        .format(
            &hint,
            media_source,
            &Default::default(),
            &Default::default(),
        )
        .ok()?;
    let mut format = probed.format;

    format
        .metadata()
        .current()?
        .visuals()
        .iter()
        .find_map(Artwork::from_visual)
}

fn detect_format(data: &[u8]) -> Option<ArtworkFormat> {
    if is_valid_jpeg(data) {
        Some(ArtworkFormat::Jpeg)
    } else if is_valid_png(data) {
        Some(ArtworkFormat::Png)
    } else {
        None
    }
}

fn is_valid_jpeg(data: &[u8]) -> bool {
    const START: &[u8; 2] = &[0xff, 0xd8];
    const END: &[u8; 2] = &[0xff, 0xd9];
    const FRAME_MARKERS: &[u8] = &[
        0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf,
    ];

    data.starts_with(START)
        && data.ends_with(END)
        && data
            .windows(2)
            .any(|marker| marker[0] == 0xff && FRAME_MARKERS.contains(&marker[1]))
        && data.windows(2).any(|marker| marker == [0xff, 0xda])
}

fn is_valid_png(data: &[u8]) -> bool {
    if !data.starts_with(PNG_SIGNATURE) {
        return false;
    }

    let mut offset = PNG_SIGNATURE.len();
    let mut has_header = false;
    let mut has_image_data = false;

    loop {
        let Some(header_end) = offset.checked_add(8) else {
            return false;
        };
        if header_end > data.len() {
            return false;
        }

        let chunk_length =
            u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        let chunk_type = &data[offset + 4..header_end];
        let Some(chunk_data_end) = header_end.checked_add(chunk_length) else {
            return false;
        };
        let Some(chunk_end) = chunk_data_end.checked_add(4) else {
            return false;
        };
        if chunk_end > data.len() {
            return false;
        }

        let expected_crc = u32::from_be_bytes(data[chunk_data_end..chunk_end].try_into().unwrap());
        if png_crc32(&data[offset + 4..chunk_data_end]) != expected_crc {
            return false;
        }

        match chunk_type {
            b"IHDR" if !has_header && chunk_length == 13 => {
                let width =
                    u32::from_be_bytes(data[header_end..header_end + 4].try_into().unwrap());
                let height =
                    u32::from_be_bytes(data[header_end + 4..header_end + 8].try_into().unwrap());
                if width == 0 || height == 0 {
                    return false;
                }
                has_header = true;
            }
            b"IHDR" => return false,
            b"IDAT" if has_header && chunk_length > 0 => has_image_data = true,
            b"IEND" => return has_header && has_image_data && chunk_length == 0,
            _ if !has_header => return false,
            _ => {}
        }

        offset = chunk_end;
    }
}

fn png_crc32(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb8_8320 } else { 0 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
    };

    use symphonia::core::meta::Visual;

    use crate::{audio::AudioMetadata, library::MusicLibrary};

    use super::{Artwork, ArtworkCache, ArtworkFormat, load_artwork_for_track, png_crc32};

    static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("aurelis-artwork-{}-{sequence}", std::process::id()));
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

    fn track(path: &Path) -> crate::library::Track {
        let mut library = MusicLibrary::new();
        let id = library.add_track(path.to_path_buf(), AudioMetadata::default());
        library.get(id).expect("track was added").clone()
    }

    fn jpeg_bytes() -> Vec<u8> {
        vec![
            0xff, 0xd8, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11,
            0x00, 0xff, 0xda, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3f, 0x00, 0x00, 0xff, 0xd9,
        ]
    }

    fn png_bytes() -> Vec<u8> {
        fn append_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            output.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let crc_start = output.len();
            output.extend_from_slice(kind);
            output.extend_from_slice(data);
            output.extend_from_slice(&png_crc32(&output[crc_start..]).to_be_bytes());
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

    #[test]
    fn returns_none_when_track_has_no_artwork() {
        let directory = TemporaryDirectory::new();
        let track_path = directory.path().join("song.flac");
        fs::write(&track_path, b"not an audio file").expect("write fake audio file");

        assert!(load_artwork_for_track(&track(&track_path)).is_none());
    }

    #[test]
    fn converts_embedded_jpeg_and_png_visuals() {
        for (media_type, bytes, expected_format) in [
            ("image/jpeg", jpeg_bytes(), ArtworkFormat::Jpeg),
            ("image/png", png_bytes(), ArtworkFormat::Png),
        ] {
            let visual = Visual {
                media_type: media_type.to_owned(),
                dimensions: None,
                bits_per_pixel: None,
                color_mode: None,
                usage: Default::default(),
                tags: Vec::new(),
                data: bytes.into_boxed_slice(),
            };

            let artwork = Artwork::from_visual(&visual).expect("valid embedded artwork");
            assert_eq!(artwork.format(), expected_format);
            assert_eq!(artwork.mime_type(), media_type);
        }
    }

    #[test]
    fn discovers_case_insensitive_folder_artwork_names() {
        let directory = TemporaryDirectory::new();
        let track_path = directory.path().join("song.flac");
        let cover_path = directory.path().join("CoVeR.PnG");
        fs::write(&track_path, b"not an audio file").expect("write fake audio file");
        fs::write(&cover_path, png_bytes()).expect("write PNG cover");

        let artwork = load_artwork_for_track(&track(&track_path)).expect("discover folder cover");
        assert_eq!(artwork.format(), ArtworkFormat::Png);
        assert_eq!(artwork.extension(), "png");
    }

    #[test]
    fn prefers_cover_name_and_falls_back_from_corrupt_images() {
        let directory = TemporaryDirectory::new();
        let track_path = directory.path().join("song.flac");
        fs::write(&track_path, b"not an audio file").expect("write fake audio file");
        fs::write(directory.path().join("cover.png"), b"corrupt image")
            .expect("write corrupt cover");
        fs::write(directory.path().join("folder.jpg"), jpeg_bytes())
            .expect("write fallback artwork");

        let artwork = load_artwork_for_track(&track(&track_path)).expect("use valid fallback");
        assert_eq!(artwork.format(), ArtworkFormat::Jpeg);
    }

    #[test]
    fn rejects_truncated_images_and_bad_png_crc() {
        let mut jpeg = jpeg_bytes();
        jpeg.pop();
        assert!(Artwork::from_bytes(jpeg).is_none());

        let mut png = png_bytes();
        let last_byte = png.len() - 1;
        png[last_byte] ^= 0x01;
        assert!(Artwork::from_bytes(png).is_none());
    }

    #[test]
    fn cache_shares_external_artwork_and_can_be_cleared() {
        let directory = TemporaryDirectory::new();
        let first_path = directory.path().join("first.flac");
        let second_path = directory.path().join("second.flac");
        let cover_path = directory.path().join("cover.png");
        fs::write(&first_path, b"not an audio file").expect("write first fake track");
        fs::write(&second_path, b"not an audio file").expect("write second fake track");
        fs::write(&cover_path, png_bytes()).expect("write PNG cover");

        let first_track = track(&first_path);
        let second_track = track(&second_path);
        let mut cache = ArtworkCache::new();
        let first = cache
            .get_for_track(&first_track)
            .expect("load first track artwork");
        fs::write(&cover_path, jpeg_bytes()).expect("replace cover");
        let second = cache
            .get_for_track(&second_track)
            .expect("reuse cached cover for second track");

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(second.format(), ArtworkFormat::Png);

        cache.clear();
        let refreshed = cache
            .get_for_track(&first_track)
            .expect("reload after clearing cache");
        assert_eq!(refreshed.format(), ArtworkFormat::Jpeg);
    }

    #[test]
    fn cache_remembers_missing_artwork_until_cleared() {
        let directory = TemporaryDirectory::new();
        let track_path = directory.path().join("song.flac");
        fs::write(&track_path, b"not an audio file").expect("write fake audio file");
        let track = track(&track_path);
        let mut cache = ArtworkCache::new();

        assert!(cache.get_for_track(&track).is_none());
        fs::write(directory.path().join("cover.jpg"), jpeg_bytes()).expect("add artwork");
        assert!(cache.get_for_track(&track).is_none());

        cache.clear();
        assert!(cache.get_for_track(&track).is_some());
    }

    #[test]
    fn cache_reuses_folder_discovery_until_cleared() {
        let directory = TemporaryDirectory::new();
        let first_path = directory.path().join("first.flac");
        let second_path = directory.path().join("second.flac");
        fs::write(&first_path, b"not an audio file").expect("write first fake track");
        fs::write(&second_path, b"not an audio file").expect("write second fake track");

        let first_track = track(&first_path);
        let second_track = track(&second_path);
        let mut cache = ArtworkCache::new();
        assert!(cache.get_for_track(&first_track).is_none());

        fs::write(directory.path().join("cover.jpg"), jpeg_bytes()).expect("add artwork");
        assert!(cache.get_for_track(&second_track).is_none());

        cache.clear();
        assert!(cache.get_for_track(&second_track).is_some());
    }
}
