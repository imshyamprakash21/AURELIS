use std::{
    error::Error,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

use symphonia::core::{io::MediaSourceStream, meta::Visual, probe::Hint};
use symphonia::default::get_probe;

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// The encoded image format retained by AURELIS.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtworkFormat {
    /// JPEG image bytes.
    Jpeg,
    /// PNG image bytes.
    Png,
}

impl ArtworkFormat {
    /// MIME type corresponding to this encoded image format.
    pub const fn mime_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
        }
    }

    /// Conventional file extension corresponding to this format.
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
        }
    }
}

/// The origin of an artwork image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArtworkSource {
    /// Image data stored in the audio file's metadata.
    Embedded,
    /// An image discovered in the audio file's directory.
    FileSystem {
        /// Path of the discovered image.
        path: PathBuf,
    },
}

/// Validated, encoded artwork data.
///
/// A missing image is represented by None. Pixel decoding is left to the
/// consumer; this model retains only supported JPEG or PNG bytes and source
/// information.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlbumArt {
    data: Box<[u8]>,
    format: ArtworkFormat,
    source: ArtworkSource,
}

impl AlbumArt {
    /// Encoded image bytes.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Detected format of the encoded image.
    pub const fn format(&self) -> ArtworkFormat {
        self.format
    }

    /// MIME type corresponding to the detected image format.
    pub const fn mime_type(&self) -> &'static str {
        self.format.mime_type()
    }

    /// Conventional extension corresponding to the detected image format.
    pub const fn extension(&self) -> &'static str {
        self.format.extension()
    }

    /// Origin of this artwork image.
    pub fn source(&self) -> &ArtworkSource {
        &self.source
    }

    /// Save the encoded image bytes to a file.
    pub fn save<P: AsRef<Path>>(&self, output_path: P) -> io::Result<()> {
        fs::write(output_path, &self.data)
    }

    fn from_bytes(data: Vec<u8>, source: ArtworkSource) -> Option<Self> {
        let format = detect_format(&data)?;
        Some(Self {
            data: data.into_boxed_slice(),
            format,
            source,
        })
    }

    fn from_visual(visual: &Visual) -> Option<Self> {
        Self::from_bytes(visual.data.to_vec(), ArtworkSource::Embedded)
    }
}

/// Extract the first supported embedded image from an audio file.
///
/// The decoder streaming API does not expose container visuals, so this
/// performs a metadata-only Symphonia probe when artwork is explicitly
/// requested. Ok(None) means the file has no supported embedded image.
/// File and probe errors are returned to explicit callers; optional library
/// lookups may ignore them so artwork problems never prevent playback.
pub fn extract_album_art<P: AsRef<Path>>(path: P) -> Result<Option<AlbumArt>, Box<dyn Error>> {
    let path = path.as_ref();
    let file = File::open(path)?;
    let media_source = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();

    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        hint.with_extension(extension);
    }

    let probed = get_probe().format(
        &hint,
        media_source,
        &Default::default(),
        &Default::default(),
    )?;
    let mut format = probed.format;

    let artwork = format
        .metadata()
        .current()
        .and_then(|revision| revision.visuals().iter().find_map(AlbumArt::from_visual));

    Ok(artwork)
}

pub(crate) fn read_artwork_file(path: &Path) -> Option<AlbumArt> {
    let data = fs::read(path).ok()?;
    AlbumArt::from_bytes(
        data,
        ArtworkSource::FileSystem {
            path: path.to_path_buf(),
        },
    )
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
    use super::*;
    use symphonia::core::meta::Visual;

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

        let mut bytes = PNG_SIGNATURE.to_vec();
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

    fn visual(media_type: &str, data: Vec<u8>) -> Visual {
        Visual {
            media_type: media_type.to_owned(),
            dimensions: None,
            bits_per_pixel: None,
            color_mode: None,
            usage: Default::default(),
            tags: Vec::new(),
            data: data.into_boxed_slice(),
        }
    }

    #[test]
    fn normalizes_embedded_jpeg_and_png_formats_and_sources() {
        for (media_type, data, expected_format, expected_extension) in [
            ("image/jpeg", jpeg_bytes(), ArtworkFormat::Jpeg, "jpg"),
            ("image/png", png_bytes(), ArtworkFormat::Png, "png"),
        ] {
            let artwork = AlbumArt::from_visual(&visual(media_type, data.clone()))
                .expect("valid embedded art");

            assert_eq!(artwork.data(), data);
            assert_eq!(artwork.format(), expected_format);
            assert_eq!(artwork.mime_type(), media_type);
            assert_eq!(artwork.extension(), expected_extension);
            assert_eq!(artwork.source(), &ArtworkSource::Embedded);
        }
    }

    #[test]
    fn classifies_discovered_artwork_with_its_source_path() {
        let path = PathBuf::from("AlbumArt.PNG");
        let artwork = AlbumArt::from_bytes(
            png_bytes(),
            ArtworkSource::FileSystem { path: path.clone() },
        )
        .expect("valid filesystem artwork");

        assert_eq!(
            artwork.source(),
            &ArtworkSource::FileSystem { path: path.clone() }
        );
        assert_eq!(artwork.format(), ArtworkFormat::Png);
    }

    #[test]
    fn rejects_truncated_jpegs_and_pngs_with_bad_crc() {
        let mut jpeg = jpeg_bytes();
        jpeg.pop();
        assert!(AlbumArt::from_visual(&visual("image/jpeg", jpeg)).is_none());

        let mut png = png_bytes();
        let last_byte = png.len() - 1;
        png[last_byte] ^= 0x01;
        assert!(AlbumArt::from_visual(&visual("image/png", png)).is_none());
    }

    #[test]
    fn unsupported_image_data_is_not_artwork() {
        assert!(AlbumArt::from_visual(&visual("image/webp", b"not an image".to_vec())).is_none());
    }

    #[test]
    fn explicit_extraction_returns_io_errors() {
        let path = std::env::temp_dir().join(format!(
            "aurelis-missing-album-art-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));

        assert!(extract_album_art(path).is_err());
    }
}
