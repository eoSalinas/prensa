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

use crate::Options;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    Jpeg,
    Png,
}

pub(crate) fn kind_of(name: &str) -> Option<Kind> {
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
pub(crate) fn is_progressive_jpeg(data: &[u8]) -> bool {
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
fn to_screen_gray(img: &DynamicImage, opts: &Options) -> GrayImage {
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

    let resized = gray.width() > opts.max_width || gray.height() > opts.max_height;
    let gray = if resized {
        // `resize` keeps the aspect ratio and fits inside the box.
        DynamicImage::ImageLuma8(gray).resize(opts.max_width, opts.max_height, FilterType::Lanczos3).into_luma8()
    } else {
        gray
    };

    // Sharpen only after downscaling (offsets Lanczos softening).
    if resized && opts.sharpen > 0.0 {
        image::imageops::unsharpen(&gray, opts.sharpen, 2)
    } else {
        gray
    }
}

fn encode(kind: Kind, img: &GrayImage, opts: &Options) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let (w, h) = img.dimensions();
    match kind {
        // The `image` crate's JPEG encoder only emits baseline.
        Kind::Jpeg => JpegEncoder::new_with_quality(&mut out, opts.jpeg_quality).write_image(img.as_raw(), w, h, ExtendedColorType::L8)?,
        Kind::Png => PngEncoder::new_with_quality(&mut out, CompressionType::Best, PngFilter::Adaptive)
            .write_image(img.as_raw(), w, h, ExtendedColorType::L8)?,
    }
    Ok(out)
}

/// Returns `Ok(None)` when the original should be kept as-is.
pub(crate) fn optimize(kind: Kind, data: &[u8], opts: &Options) -> Result<Option<Vec<u8>>> {
    let format = match kind {
        Kind::Jpeg => ImageFormat::Jpeg,
        Kind::Png => ImageFormat::Png,
    };
    let img = image::load_from_memory_with_format(data, format)?;

    // Cases where the change is required even if the file grows.
    let too_big = img.width() > opts.max_width || img.height() > opts.max_height;
    let progressive = kind == Kind::Jpeg && is_progressive_jpeg(data);

    let out = encode(kind, &to_screen_gray(&img, opts), opts)?;
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
        let out = optimize(Kind::Jpeg, &colorful_jpeg(2000, 1000), &Options::default()).unwrap().unwrap();
        let img = decode(&out);
        assert_eq!((img.width(), img.height()), (440, 220));
        assert!(!img.color().has_color());
        assert!(!is_progressive_jpeg(&out));
    }

    #[test]
    fn never_upscales() {
        let out = optimize(Kind::Jpeg, &colorful_jpeg(100, 50), &Options::default()).unwrap();
        if let Some(out) = out {
            assert_eq!(decode(&out).width(), 100);
        }
    }

    #[test]
    fn transparency_becomes_white() {
        let img = RgbaImage::from_pixel(10, 10, Rgba([0, 0, 0, 0]));
        let mut png = Vec::new();
        DynamicImage::ImageRgba8(img).write_to(&mut Cursor::new(&mut png), ImageFormat::Png).unwrap();
        let out = optimize(Kind::Png, &png, &Options::default()).unwrap().unwrap();
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
