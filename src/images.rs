//! `images` pass: fit each image to the e-reader screen.
//!
//! - downscale to fit the configured box (aspect ratio kept; never upscales);
//! - convert to grayscale (e-ink shows no color), flattening transparency onto white;
//! - JPEG output is always baseline: CrossPoint decodes progressive JPEG at 1/8
//!   resolution, so the image comes out blurry;
//! - never makes things worse: if the result is larger and the image did not
//!   *need* to change, the original is kept.

use anyhow::Result;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, GrayImage, ImageEncoder, ImageFormat, Luma};

// Defaults chosen on an X4 Pro (CrossPoint) by comparing Q85, Q92, Q92+sharpen and
// Q92+sharpen+440 on the same diagram page: the last one won.
// 440x733 sits close to the reader viewport (480 minus bezel and margins), so the
// device does not resample again with its low-quality fixed-point scaler.
pub const DEFAULT_MAX_WIDTH: u32 = 440;
pub const DEFAULT_MAX_HEIGHT: u32 = 733;
pub const DEFAULT_JPEG_QUALITY: u8 = 92;
pub const DEFAULT_SHARPEN: f32 = 0.8;

/// Pass parameters.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub max_width: u32,
    pub max_height: u32,
    pub jpeg_quality: u8,
    /// Unsharp mask sigma applied after downscaling. `0` disables sharpening.
    pub sharpen: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_width: DEFAULT_MAX_WIDTH,
            max_height: DEFAULT_MAX_HEIGHT,
            jpeg_quality: DEFAULT_JPEG_QUALITY,
            sharpen: DEFAULT_SHARPEN,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Jpeg,
    Png,
}

pub fn kind_of(name: &str) -> Option<Kind> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some(Kind::Jpeg)
    } else if lower.ends_with(".png") {
        Some(Kind::Png)
    } else {
        None
    }
}

/// Look for the SOF marker: `FFC2` = progressive, `FFC0`/`FFC1` = baseline.
pub fn is_progressive_jpeg(data: &[u8]) -> bool {
    let mut i = 2; // skip SOI (FFD8)
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        match data[i + 1] {
            0xC2 => return true,
            0xC0 | 0xC1 => return false,
            _ => {
                let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
                i += 2 + len;
            }
        }
    }
    false
}

/// Grayscale with alpha flattened onto white, downscaled to fit the box.
fn to_screen_gray(img: &DynamicImage, cfg: &Config) -> GrayImage {
    let gray = if img.color().has_alpha() {
        let rgba = img.to_rgba8();
        GrayImage::from_fn(img.width(), img.height(), |x, y| {
            let [r, g, b, a] = rgba.get_pixel(x, y).0;
            let luma = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
            let alpha = a as f32 / 255.0;
            Luma([(luma * alpha + 255.0 * (1.0 - alpha)).round() as u8])
        })
    } else {
        img.to_luma8()
    };

    let resized = gray.width() > cfg.max_width || gray.height() > cfg.max_height;
    let gray = if resized {
        // `resize` keeps the aspect ratio and fits inside the box.
        DynamicImage::ImageLuma8(gray).resize(cfg.max_width, cfg.max_height, FilterType::Lanczos3).into_luma8()
    } else {
        gray
    };

    // Sharpen only after downscaling (offsets Lanczos softening).
    if resized && cfg.sharpen > 0.0 {
        image::imageops::unsharpen(&gray, cfg.sharpen, 2)
    } else {
        gray
    }
}

fn encode(kind: Kind, img: &GrayImage, cfg: &Config) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let (w, h) = img.dimensions();
    match kind {
        // The `image` crate's JPEG encoder only emits baseline.
        Kind::Jpeg => JpegEncoder::new_with_quality(&mut out, cfg.jpeg_quality).write_image(img.as_raw(), w, h, ExtendedColorType::L8)?,
        Kind::Png => PngEncoder::new_with_quality(&mut out, CompressionType::Best, PngFilter::Adaptive)
            .write_image(img.as_raw(), w, h, ExtendedColorType::L8)?,
    }
    Ok(out)
}

/// Returns `Ok(None)` when the original should be kept as-is.
pub fn optimize(kind: Kind, data: &[u8], cfg: &Config) -> Result<Option<Vec<u8>>> {
    let format = match kind {
        Kind::Jpeg => ImageFormat::Jpeg,
        Kind::Png => ImageFormat::Png,
    };
    let img = image::load_from_memory_with_format(data, format)?;

    // Cases where the change is required even if the file grows.
    let too_big = img.width() > cfg.max_width || img.height() > cfg.max_height;
    let progressive = kind == Kind::Jpeg && is_progressive_jpeg(data);

    let out = encode(kind, &to_screen_gray(&img, cfg), cfg)?;
    if out.len() >= data.len() && !too_big && !progressive {
        return Ok(None);
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, Rgba, RgbaImage};
    use std::io::Cursor;

    fn decode(data: &[u8]) -> DynamicImage {
        image::load_from_memory(data).unwrap()
    }

    fn colorful_jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = RgbImage::from_fn(w, h, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, ((x * y) % 256) as u8]));
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut out), ImageFormat::Jpeg).unwrap();
        out
    }

    #[test]
    fn downscales_to_fit_keeping_aspect_ratio() {
        let out = optimize(Kind::Jpeg, &colorful_jpeg(2000, 1000), &Config::default()).unwrap().unwrap();
        let img = decode(&out);
        assert_eq!((img.width(), img.height()), (440, 220));
        assert!(!img.color().has_color());
        assert!(!is_progressive_jpeg(&out));
    }

    #[test]
    fn never_upscales() {
        let out = optimize(Kind::Jpeg, &colorful_jpeg(100, 50), &Config::default()).unwrap();
        if let Some(out) = out {
            assert_eq!(decode(&out).width(), 100);
        }
    }

    #[test]
    fn transparency_becomes_white() {
        let img = RgbaImage::from_pixel(10, 10, Rgba([0, 0, 0, 0]));
        let mut png = Vec::new();
        DynamicImage::ImageRgba8(img).write_to(&mut Cursor::new(&mut png), ImageFormat::Png).unwrap();
        let out = optimize(Kind::Png, &png, &Config::default()).unwrap().unwrap();
        let gray = decode(&out).to_luma8();
        assert!(gray.pixels().all(|p| p.0[0] == 255));
    }

    #[test]
    fn detects_progressive_jpeg() {
        // SOI, SOF2 (progressive) with minimal length.
        assert!(is_progressive_jpeg(&[0xFF, 0xD8, 0xFF, 0xC2, 0x00, 0x02]));
        assert!(!is_progressive_jpeg(&colorful_jpeg(8, 8)));
    }

    #[test]
    fn kind_from_name() {
        assert_eq!(kind_of("a/B.JPEG"), Some(Kind::Jpeg));
        assert_eq!(kind_of("x.png"), Some(Kind::Png));
        assert_eq!(kind_of("x.svg"), None);
    }
}
