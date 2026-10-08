use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use super::Track;
use crate::audio::album_art::{extract_album_art, read_artwork_file};

pub use crate::audio::album_art::{AlbumArt as Artwork, ArtworkFormat, ArtworkSource};

const EXPLICIT_ARTWORK_NAMES: &[&str] = &[
    "albumart.jpg",
    "albumart.jpeg",
    "albumart.png",
    "album-art.jpg",
    "album-art.jpeg",
    "album-art.png",
    "album_art.jpg",
    "album_art.jpeg",
    "album_art.png",
    "albumartsmall.jpg",
    "albumartsmall.jpeg",
    "albumartsmall.png",
];
const COMMON_ARTWORK_NAMES: &[&str] = &[
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
    "artwork.jpg",
    "artwork.jpeg",
    "artwork.png",
];

/// Loads artwork only when requested and shares cached images between tracks.
///
/// Priority is embedded artwork, explicit albumart files, then common folder
/// names (cover, folder, front, album, and artwork). Names are matched
/// case-insensitively in a fixed order. Both hits and missing results are
/// cached; call ArtworkCache::clear after files may have changed.
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
    ///
    /// Artwork failures are optional metadata and return None; they do not
    /// affect track metadata or playback.
    pub fn get_for_track(&mut self, track: &Track) -> Option<Arc<Artwork>> {
        let track_path = track.path().to_path_buf();
        if let Some(artwork) = self.by_track.get(&track_path) {
            return artwork.clone();
        }

        let embedded = extract_album_art(&track_path).ok().flatten().map(Arc::new);
        let artwork = prefer_embedded_artwork(embedded, || {
            for image_path in self.external_artwork_paths(&track_path) {
                if let Some(found) = self.external_artwork(&image_path) {
                    return Some(found);
                }
            }
            None
        });

        self.by_track.insert(track_path, artwork.clone());
        artwork
    }

    /// Forget cached artwork, candidate lists, and cached misses.
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

        let artwork = read_artwork_file(path).map(Arc::new);
        self.by_external_file.insert(key, artwork.clone());
        artwork
    }
}

/// Load artwork for a track without retaining a cache.
///
/// Embedded images take priority over filesystem artwork. File, probe, and
/// image validation failures are treated as missing optional artwork.
pub fn load_artwork_for_track(track: &Track) -> Option<Artwork> {
    let embedded = extract_album_art(track.path()).ok().flatten();
    prefer_embedded_artwork(embedded, || {
        external_artwork_paths(track.path())
            .iter()
            .find_map(|path| read_artwork_file(path))
    })
}

fn prefer_embedded_artwork<T>(
    embedded: Option<T>,
    filesystem_artwork: impl FnOnce() -> Option<T>,
) -> Option<T> {
    embedded.or_else(filesystem_artwork)
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
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        (name.to_ascii_lowercase(), name)
    });

    EXPLICIT_ARTWORK_NAMES
        .iter()
        .chain(COMMON_ARTWORK_NAMES)
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

    use crate::{
        audio::{AudioMetadata, album_art::ArtworkSource},
        library::MusicLibrary,
    };

    use super::{ArtworkCache, ArtworkFormat, load_artwork_for_track};

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
            output.extend_from_slice(&test_png_crc32(&output[crc_start..]).to_be_bytes());
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

    fn test_png_crc32(data: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for byte in data {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb8_8320 } else { 0 };
            }
        }
        !crc
    }

    fn flac_with_embedded_png(image_data: &[u8]) -> Vec<u8> {
        let mut bytes = b"fLaC".to_vec();

        // STREAMINFO: 44.1 kHz, mono, 16-bit, with a legal block size.
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 34]);
        bytes.extend_from_slice(&4096_u16.to_be_bytes());
        bytes.extend_from_slice(&4096_u16.to_be_bytes());
        bytes.extend_from_slice(&[0; 6]);
        let stream_info = (44_100_u64 << 44) | (15_u64 << 36);
        bytes.extend_from_slice(&stream_info.to_be_bytes());
        bytes.extend_from_slice(&[0; 16]);

        let mut picture = Vec::new();
        picture.extend_from_slice(&3_u32.to_be_bytes());
        let mime = b"image/png";
        picture.extend_from_slice(&(mime.len() as u32).to_be_bytes());
        picture.extend_from_slice(mime);
        picture.extend_from_slice(&0_u32.to_be_bytes());
        picture.extend_from_slice(&1_u32.to_be_bytes());
        picture.extend_from_slice(&1_u32.to_be_bytes());
        picture.extend_from_slice(&32_u32.to_be_bytes());
        picture.extend_from_slice(&0_u32.to_be_bytes());
        picture.extend_from_slice(&(image_data.len() as u32).to_be_bytes());
        picture.extend_from_slice(image_data);

        bytes.push(0x86);
        let picture_len = picture.len();
        bytes.extend_from_slice(&[
            ((picture_len >> 16) & 0xff) as u8,
            ((picture_len >> 8) & 0xff) as u8,
            (picture_len & 0xff) as u8,
        ]);
        bytes.extend_from_slice(&picture);

        // A valid frame header lets Symphonia finish probing the FLAC stream.
        let frame_start = bytes.len();
        bytes.extend_from_slice(&[0xff, 0xf8, 0x10, 0x00, 0x00]);
        let crc = flac_crc8(&bytes[frame_start..]);
        bytes.push(crc);
        bytes
    }

    fn flac_crc8(data: &[u8]) -> u8 {
        let mut crc = 0_u8;
        for byte in data {
            crc ^= byte;
            for _ in 0..8 {
                crc = if crc & 0x80 == 0 {
                    crc << 1
                } else {
                    (crc << 1) ^ 0x07
                };
            }
        }
        crc
    }
    #[test]
    fn returns_none_when_track_has_no_artwork() {
        let directory = TemporaryDirectory::new();
        let track_path = directory.path().join("song.flac");
        fs::write(&track_path, b"not an audio file").expect("write fake audio file");

        assert!(load_artwork_for_track(&track(&track_path)).is_none());
    }

    #[test]
    fn discovers_case_insensitive_folder_artwork_names_and_classifies_source() {
        let directory = TemporaryDirectory::new();
        let track_path = directory.path().join("song.flac");
        let cover_path = directory.path().join("CoVeR.PnG");
        fs::write(&track_path, b"not an audio file").expect("write fake audio file");
        fs::write(&cover_path, png_bytes()).expect("write PNG cover");

        let artwork = load_artwork_for_track(&track(&track_path)).expect("discover folder cover");
        assert_eq!(artwork.format(), ArtworkFormat::Png);
        assert_eq!(
            artwork.source(),
            &ArtworkSource::FileSystem { path: cover_path }
        );
    }

    #[test]
    fn explicit_album_art_precedes_common_folder_names_deterministically() {
        let directory = TemporaryDirectory::new();
        let track_path = directory.path().join("song.flac");
        let explicit_path = directory.path().join("albumart.jpg");
        fs::write(&track_path, b"not an audio file").expect("write fake audio file");
        fs::write(&explicit_path, jpeg_bytes()).expect("write explicit JPEG");
        fs::write(directory.path().join("cover.png"), png_bytes()).expect("write folder PNG");
        fs::write(directory.path().join("folder.png"), png_bytes()).expect("write folder PNG");

        let artwork = load_artwork_for_track(&track(&track_path)).expect("find artwork");
        assert_eq!(artwork.format(), ArtworkFormat::Jpeg);
        assert_eq!(
            artwork.source(),
            &ArtworkSource::FileSystem {
                path: explicit_path
            }
        );
    }

    #[test]
    fn invalid_artwork_like_file_is_skipped_for_next_valid_candidate() {
        let directory = TemporaryDirectory::new();
        let track_path = directory.path().join("song.flac");
        let cover_path = directory.path().join("cover.jpg");
        fs::write(&track_path, b"not an audio file").expect("write fake audio file");
        fs::write(directory.path().join("albumart.jpg"), b"not an image")
            .expect("write invalid explicit artwork");
        fs::write(&cover_path, jpeg_bytes()).expect("write valid cover");

        let artwork = load_artwork_for_track(&track(&track_path)).expect("use valid cover");
        assert_eq!(
            artwork.source(),
            &ArtworkSource::FileSystem { path: cover_path }
        );
    }

    #[test]
    fn embedded_artwork_precedes_explicit_and_common_files() {
        let directory = TemporaryDirectory::new();
        let track_path = directory.path().join("song.flac");
        let explicit_path = directory.path().join("albumart.jpg");
        let embedded_png = png_bytes();
        fs::write(&track_path, flac_with_embedded_png(&embedded_png))
            .expect("write FLAC with embedded artwork");
        fs::write(&explicit_path, jpeg_bytes()).expect("write explicit JPEG");
        fs::write(directory.path().join("cover.jpg"), jpeg_bytes())
            .expect("write common folder artwork");

        let artwork = load_artwork_for_track(&track(&track_path)).expect("extract embedded art");

        assert_eq!(artwork.source(), &ArtworkSource::Embedded);
        assert_eq!(artwork.format(), ArtworkFormat::Png);
        assert_eq!(artwork.data(), embedded_png);
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
        assert_eq!(
            second.source(),
            &ArtworkSource::FileSystem { path: cover_path }
        );

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
