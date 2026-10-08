use std::{error::Error, fmt};

const COLOR_BUCKET_COUNT: usize = 1 << 12;

/// An sRGB color with 8-bit red, green, and blue channels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RgbColor {
    red: u8,
    green: u8,
    blue: u8,
}

impl RgbColor {
    /// Create a color from 8-bit sRGB channels.
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    /// Red channel.
    pub const fn red(self) -> u8 {
        self.red
    }

    /// Green channel.
    pub const fn green(self) -> u8 {
        self.green
    }

    /// Blue channel.
    pub const fn blue(self) -> u8 {
        self.blue
    }
}

/// The two colors to use at the ends of a background gradient.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackgroundGradient {
    start: RgbColor,
    end: RgbColor,
}

impl BackgroundGradient {
    /// Gradient start color, selected from the dominant artwork color.
    pub const fn start(self) -> RgbColor {
        self.start
    }

    /// Gradient end color, selected from the accent artwork color.
    pub const fn end(self) -> RgbColor {
        self.end
    }
}

/// Recommended brightness for text shown over the artwork-derived gradient.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextContrast {
    /// Use light text, typically white.
    Light,
    /// Use dark text, typically black.
    Dark,
}

impl TextContrast {
    /// A standard white or black text color with the stronger contrast over
    /// both gradient endpoints.
    pub const fn text_color(self) -> RgbColor {
        match self {
            Self::Light => RgbColor::new(255, 255, 255),
            Self::Dark => RgbColor::new(0, 0, 0),
        }
    }
}

/// A small palette model derived from decoded artwork pixels.
///
/// A presentation consumer decodes the encoded [`crate::library::Artwork`]
/// and passes a small, representative RGBA sample to [`Self::from_rgba_pixels`].
/// Keeping pixel decoding outside this model avoids coupling the application
/// to a particular image decoder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtworkVisual {
    dominant_color: RgbColor,
    accent_color: RgbColor,
    background_gradient: BackgroundGradient,
    text_contrast: TextContrast,
}

impl ArtworkVisual {
    /// Derive palette information from decoded RGBA pixels.
    ///
    /// Pixels with zero alpha are ignored. Colors are grouped into 4-bit RGB
    /// buckets; the most common bucket is the dominant color and the next
    /// distinct bucket is the accent color. Pass a downsampled image to keep
    /// this operation small and suitable for presentation work.
    pub fn from_rgba_pixels(pixels: &[[u8; 4]]) -> Result<Self, ArtworkVisualError> {
        let mut buckets = vec![ColorBucket::default(); COLOR_BUCKET_COUNT];

        for [red, green, blue, alpha] in pixels.iter().copied() {
            if alpha == 0 {
                continue;
            }

            let index = (usize::from(red >> 4) << 8)
                | (usize::from(green >> 4) << 4)
                | usize::from(blue >> 4);
            let bucket = &mut buckets[index];
            let weight = u64::from(alpha);
            bucket.weight += weight;
            bucket.red += u64::from(red) * weight;
            bucket.green += u64::from(green) * weight;
            bucket.blue += u64::from(blue) * weight;
        }

        let mut dominant = None;
        let mut accent = None;
        for (index, bucket) in buckets.iter().enumerate() {
            if bucket.weight == 0 {
                continue;
            }
            let candidate = (index, bucket.weight);
            match dominant {
                None => dominant = Some(candidate),
                Some((_, weight)) if bucket.weight > weight => {
                    accent = dominant;
                    dominant = Some(candidate);
                }
                _ if accent.is_none_or(|(_, weight)| bucket.weight > weight) => {
                    accent = Some(candidate);
                }
                _ => {}
            }
        }

        let (dominant_index, _) = dominant.ok_or(ArtworkVisualError::NoVisiblePixels)?;
        let dominant_color = buckets[dominant_index].average_color();
        let accent_color = accent
            .map(|(index, _)| buckets[index].average_color())
            .unwrap_or(dominant_color);
        let background_gradient = BackgroundGradient {
            start: dominant_color,
            end: accent_color,
        };
        let text_contrast = choose_text_contrast(background_gradient);

        Ok(Self {
            dominant_color,
            accent_color,
            background_gradient,
            text_contrast,
        })
    }

    /// The most common quantized color in the supplied artwork samples.
    pub const fn dominant_color(self) -> RgbColor {
        self.dominant_color
    }

    /// The second most common distinct quantized color, or the dominant color
    /// when the sample contains only one color bucket.
    pub const fn accent_color(self) -> RgbColor {
        self.accent_color
    }

    /// Background gradient endpoints derived from the dominant and accent
    /// colors.
    pub const fn background_gradient(self) -> BackgroundGradient {
        self.background_gradient
    }

    /// Recommended text brightness for the gradient endpoints.
    pub const fn text_contrast(self) -> TextContrast {
        self.text_contrast
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ColorBucket {
    weight: u64,
    red: u64,
    green: u64,
    blue: u64,
}

impl ColorBucket {
    fn average_color(self) -> RgbColor {
        RgbColor::new(
            channel_average(self.red, self.weight),
            channel_average(self.green, self.weight),
            channel_average(self.blue, self.weight),
        )
    }
}

fn channel_average(total: u64, weight: u64) -> u8 {
    ((total + weight / 2) / weight) as u8
}

fn choose_text_contrast(gradient: BackgroundGradient) -> TextContrast {
    let endpoints = [gradient.start, gradient.end];
    let light_contrast = endpoints
        .into_iter()
        .map(|color| contrast_ratio(1.0, relative_luminance(color)))
        .fold(f64::INFINITY, f64::min);
    let dark_contrast = endpoints
        .into_iter()
        .map(|color| contrast_ratio(0.0, relative_luminance(color)))
        .fold(f64::INFINITY, f64::min);

    if light_contrast >= dark_contrast {
        TextContrast::Light
    } else {
        TextContrast::Dark
    }
}

fn relative_luminance(color: RgbColor) -> f64 {
    let [red, green, blue] = [color.red, color.green, color.blue].map(|channel| {
        let value = f64::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    });
    0.2126 * red + 0.7152 * green + 0.0722 * blue
}

fn contrast_ratio(text_luminance: f64, background_luminance: f64) -> f64 {
    let lighter = text_luminance.max(background_luminance);
    let darker = text_luminance.min(background_luminance);
    (lighter + 0.05) / (darker + 0.05)
}

/// An error encountered while deriving a visual model from pixel samples.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtworkVisualError {
    /// No non-transparent samples were provided.
    NoVisiblePixels,
}

impl fmt::Display for ArtworkVisualError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoVisiblePixels => formatter.write_str("artwork sample has no visible pixels"),
        }
    }
}

impl Error for ArtworkVisualError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_dominant_accent_and_gradient_colors() {
        let pixels = [
            [20, 30, 40, 255],
            [20, 30, 40, 255],
            [20, 30, 40, 255],
            [20, 30, 40, 255],
            [220, 130, 50, 255],
            [220, 130, 50, 255],
        ];

        let visual = ArtworkVisual::from_rgba_pixels(&pixels).unwrap();

        assert_eq!(visual.dominant_color(), RgbColor::new(20, 30, 40));
        assert_eq!(visual.accent_color(), RgbColor::new(220, 130, 50));
        assert_eq!(
            visual.background_gradient(),
            BackgroundGradient {
                start: RgbColor::new(20, 30, 40),
                end: RgbColor::new(220, 130, 50),
            }
        );
    }

    #[test]
    fn chooses_text_contrast_for_dark_and_light_artwork() {
        let dark = ArtworkVisual::from_rgba_pixels(&[[20, 20, 20, 255]]).unwrap();
        assert_eq!(dark.text_contrast(), TextContrast::Light);
        assert_eq!(
            dark.text_contrast().text_color(),
            RgbColor::new(255, 255, 255)
        );

        let light = ArtworkVisual::from_rgba_pixels(&[[240, 240, 240, 255]]).unwrap();
        assert_eq!(light.text_contrast(), TextContrast::Dark);
        assert_eq!(light.text_contrast().text_color(), RgbColor::new(0, 0, 0));
    }

    #[test]
    fn ignores_transparent_pixels_and_handles_single_color_samples() {
        let visual = ArtworkVisual::from_rgba_pixels(&[
            [255, 0, 0, 0],
            [32, 64, 192, 255],
            [32, 64, 192, 255],
        ])
        .unwrap();

        assert_eq!(visual.dominant_color(), RgbColor::new(32, 64, 192));
        assert_eq!(visual.accent_color(), visual.dominant_color());
        assert_eq!(
            ArtworkVisual::from_rgba_pixels(&[]),
            Err(ArtworkVisualError::NoVisiblePixels)
        );
        assert_eq!(
            ArtworkVisual::from_rgba_pixels(&[[10, 20, 30, 0]]),
            Err(ArtworkVisualError::NoVisiblePixels)
        );
    }
}
