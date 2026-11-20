//! `images` pass: fit each image to the e-reader screen.
//!
//! - downscale to fit the configured box (aspect ratio kept; never upscales);
//! - convert to grayscale (e-ink shows no color), flattening transparency onto white;
//! - JPEG output is always baseline: CrossPoint decodes progressive JPEG at 1/8
//!   resolution, so the image comes out blurry;
//! - formats CrossPoint cannot decode (WebP, BMP) become JPEG or PNG, and GIF
//!   becomes PNG when that is smaller or the image must shrink. The caller
//!   renames the file, since the reader picks a decoder from the extension;
//! - never makes things worse: if the result is larger and the image did not
//!   *need* to change, the original is kept.

use anyhow::Result;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, GrayImage, ImageEncoder, ImageFormat, Luma};

use crate::Options;

/// Format of an image entry, from its extension.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    Jpeg,
    Png,
    Gif,
    Bmp,
    Webp,
}

/// Format an image is written in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Format {
    Jpeg,
    Png,
}

impl Format {
    pub(crate) fn extension(self) -> &'static str {
        match self {
            Format::Jpeg => "jpg",
            Format::Png => "png",
        }
    }

    pub(crate) fn media_type(self) -> &'static str {
        match self {
            Format::Jpeg => "image/jpeg",
            Format::Png => "image/png",
        }
    }
}

pub(crate) fn kind_of(name: &str) -> Option<Kind> {
    let (_, ext) = name.rsplit_once('.')?;
    match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some(Kind::Jpeg),
        "png" => Some(Kind::Png),
        "gif" => Some(Kind::Gif),
        "bmp" => Some(Kind::Bmp),
        "webp" => Some(Kind::Webp),
        _ => None,
    }
}

impl Kind {
    /// Whether the output can have another format, so the file may be renamed.
    pub(crate) fn may_change_format(self) -> bool {
        !matches!(self, Kind::Jpeg | Kind::Png)
    }

    /// CrossPoint has decoders for JPEG, PNG and GIF only; other formats are
    /// left out of the page, so they must be converted.
    fn device_decodes(self) -> bool {
        matches!(self, Kind::Jpeg | Kind::Png | Kind::Gif)
    }

    fn image_format(self) -> ImageFormat {
        match self {
            Kind::Jpeg => ImageFormat::Jpeg,
            Kind::Png => ImageFormat::Png,
            Kind::Gif => ImageFormat::Gif,
            Kind::Bmp => ImageFormat::Bmp,
            Kind::Webp => ImageFormat::WebP,
        }
    }
}

/// Output format: JPEG stays JPEG and PNG stays PNG. Lossless sources (BMP,
/// GIF, lossless WebP) become PNG, so diagrams and text keep sharp edges;
/// lossy WebP, usually a photo, becomes JPEG.
fn output_format(kind: Kind, data: &[u8]) -> Format {
    match kind {
        Kind::Jpeg => Format::Jpeg,
        Kind::Png | Kind::Gif | Kind::Bmp => Format::Png,
        Kind::Webp if is_lossless_webp(data) => Format::Png,
        Kind::Webp => Format::Jpeg,
    }
}

/// Whether a WebP file holds a lossless (`VP8L`) image rather than a lossy
/// (`VP8 `) one. Walks the RIFF chunks after the 12-byte header; an extended
/// file (`VP8X`) has the image chunk further on.
fn is_lossless_webp(data: &[u8]) -> bool {
    let mut i = 12;
    while let Some(header) = data.get(i..i + 8) {
        match &header[..4] {
            b"VP8L" => return true,
            b"VP8 " => return false,
            _ => {}
        }
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        // Chunks are padded to an even size.
        i = i
            .saturating_add(8)
            .saturating_add(size)
            .saturating_add(size & 1);
    }
    false
}

/// Look for the frame header (SOF) and report whether it is progressive.
///
/// SOF markers are `FFC0`-`FFCF` except `FFC4` (DHT), `FFC8` (reserved) and
/// `FFCC` (DAC); `FFC2`, `FFC6`, `FFCA` and `FFCE` are the progressive ones.
/// Any marker may be preceded by `FF` fill bytes.
pub(crate) fn is_progressive_jpeg(data: &[u8]) -> bool {
    let mut i = 2; // skip SOI (FFD8)
    while i + 1 < data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        match data[i + 1] {
            // Fill byte: the marker is one byte further.
            0xFF => i += 1,
            0xC2 | 0xC6 | 0xCA | 0xCE => return true,
            0xC0..=0xCF if !matches!(data[i + 1], 0xC4 | 0xC8 | 0xCC) => return false,
            // Scan data or end of image before any frame header: not a JPEG we understand.
            0xDA | 0xD9 => return false,
            // Standalone markers (TEM, RSTn, SOI) have no length field.
            0x01 | 0xD0..=0xD8 => i += 2,
            _ => {
                let Some(&[hi, lo]) = data.get(i + 2..i + 4) else {
                    return false;
                };
                i += 2 + usize::from(u16::from_be_bytes([hi, lo]));
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

    let resized = gray.width() > opts.max_width() || gray.height() > opts.max_height();
    let gray = if resized {
        // `resize` keeps the aspect ratio and fits inside the box.
        DynamicImage::ImageLuma8(gray)
            .resize(opts.max_width(), opts.max_height(), FilterType::Lanczos3)
            .into_luma8()
    } else {
        gray
    };

    // Sharpen only after downscaling (offsets Lanczos softening).
    match opts.sharpen() {
        Some(sigma) if resized => image::imageops::unsharpen(&gray, sigma, 2),
        _ => gray,
    }
}

fn encode(format: Format, img: &GrayImage, opts: &Options) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let (w, h) = img.dimensions();
    match format {
        // The `image` crate's JPEG encoder only emits baseline.
        Format::Jpeg => JpegEncoder::new_with_quality(&mut out, opts.jpeg_quality()).write_image(
            img.as_raw(),
            w,
            h,
            ExtendedColorType::L8,
        )?,
        Format::Png => {
            PngEncoder::new_with_quality(&mut out, CompressionType::Best, PngFilter::Adaptive)
                .write_image(img.as_raw(), w, h, ExtendedColorType::L8)?
        }
    }
    Ok(out)
}

/// A rewritten image and the format it is now in.
#[derive(Debug)]
pub(crate) struct Optimized {
    pub(crate) bytes: Vec<u8>,
    pub(crate) format: Format,
}

/// Returns `Ok(None)` when the original should be kept as-is.
pub(crate) fn optimize(kind: Kind, data: &[u8], opts: &Options) -> Result<Option<Optimized>> {
    let img = image::load_from_memory_with_format(data, kind.image_format())?;
    let format = output_format(kind, data);

    // Cases where the change is required even if the file grows.
    let too_big = img.width() > opts.max_width() || img.height() > opts.max_height();
    let progressive = kind == Kind::Jpeg && is_progressive_jpeg(data);
    let must_change = too_big || progressive || !kind.device_decodes();

    let bytes = encode(format, &to_screen_gray(&img, opts), opts)?;
    if bytes.len() >= data.len() && !must_change {
        return Ok(None);
    }
    Ok(Some(Optimized { bytes, format }))
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
        let img = RgbImage::from_fn(w, h, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, ((x * y) % 256) as u8])
        });
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Jpeg)
            .unwrap();
        out
    }

    #[test]
    fn downscales_to_fit_keeping_aspect_ratio() {
        let out = optimize(Kind::Jpeg, &colorful_jpeg(2000, 1000), &Options::default())
            .unwrap()
            .unwrap();
        assert_eq!(out.format, Format::Jpeg);
        let out = out.bytes;
        let img = decode(&out);
        assert_eq!((img.width(), img.height()), (440, 220));
        assert!(!img.color().has_color());
        assert!(!is_progressive_jpeg(&out));
    }

    #[test]
    fn never_upscales() {
        // Tested below `optimize`, which may keep a small image untouched and
        // then has no output to check.
        let small = decode(&colorful_jpeg(100, 50));
        let out = to_screen_gray(&small, &Options::default());
        assert_eq!(out.dimensions(), (100, 50));
    }

    #[test]
    fn fits_a_tall_image_by_height() {
        let tall = decode(&colorful_jpeg(500, 2000));
        let out = to_screen_gray(&tall, &Options::default());
        assert_eq!(out.dimensions(), (183, 733));
    }

    #[test]
    fn transparency_becomes_white() {
        let img = RgbaImage::from_pixel(10, 10, Rgba([0, 0, 0, 0]));
        let mut png = Vec::new();
        DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .unwrap();
        let out = optimize(Kind::Png, &png, &Options::default())
            .unwrap()
            .unwrap();
        let gray = decode(&out.bytes).to_luma8();
        assert!(gray.pixels().all(|p| p.0[0] == 255));
    }

    #[test]
    fn detects_progressive_jpeg() {
        // SOI, SOF2 (progressive) with minimal length.
        assert!(is_progressive_jpeg(&[0xFF, 0xD8, 0xFF, 0xC2, 0x00, 0x02]));
        assert!(!is_progressive_jpeg(&colorful_jpeg(8, 8)));
    }

    #[test]
    fn detects_progressive_jpeg_after_fill_bytes() {
        // SOI, APP0 (length 4), fill bytes, SOF2. The old parser read the fill
        // byte as a marker, took `FF C2` as its length and skipped the SOF.
        let data = [
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xC2, 0x00, 0x02,
        ];
        assert!(is_progressive_jpeg(&data));
    }

    #[test]
    fn detects_every_progressive_sof() {
        for marker in [0xC2, 0xC6, 0xCA, 0xCE] {
            assert!(is_progressive_jpeg(&[0xFF, 0xD8, 0xFF, marker, 0x00, 0x02]));
        }
        for marker in [0xC0, 0xC1, 0xC3, 0xC9] {
            assert!(!is_progressive_jpeg(&[
                0xFF, 0xD8, 0xFF, marker, 0x00, 0x02
            ]));
        }
    }

    #[test]
    fn skips_tables_before_the_frame_header() {
        // DHT (FFC4) is in the SOF range but is not a SOF.
        let data = [0xFF, 0xD8, 0xFF, 0xC4, 0x00, 0x02, 0xFF, 0xC2, 0x00, 0x02];
        assert!(is_progressive_jpeg(&data));
    }

    #[test]
    fn truncated_jpeg_is_not_progressive() {
        assert!(!is_progressive_jpeg(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00]));
        assert!(!is_progressive_jpeg(&[0xFF, 0xD8, 0xFF]));
        assert!(!is_progressive_jpeg(&[]));
    }

    #[test]
    fn kind_from_name() {
        assert_eq!(kind_of("a/B.JPEG"), Some(Kind::Jpeg));
        assert_eq!(kind_of("x.png"), Some(Kind::Png));
        assert_eq!(kind_of("x.GIF"), Some(Kind::Gif));
        assert_eq!(kind_of("x.bmp"), Some(Kind::Bmp));
        assert_eq!(kind_of("x.webp"), Some(Kind::Webp));
        assert_eq!(kind_of("x.svg"), None);
        assert_eq!(kind_of("webp"), None);
    }

    fn encoded(img: DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), format).unwrap();
        out
    }

    fn colorful(w: u32, h: u32) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_fn(w, h, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, ((x * y) % 256) as u8])
        }))
    }

    #[test]
    fn bmp_becomes_png_even_when_small() {
        // A tiny BMP: the PNG may not be smaller, but CrossPoint cannot show BMP.
        let bmp = encoded(colorful(4, 4), ImageFormat::Bmp);
        let out = optimize(Kind::Bmp, &bmp, &Options::default())
            .unwrap()
            .expect("converted");
        assert_eq!(out.format, Format::Png);
        let img = decode(&out.bytes);
        assert_eq!((img.width(), img.height()), (4, 4));
        assert!(!img.color().has_color());
    }

    #[test]
    fn lossless_webp_becomes_png() {
        // The `image` crate only encodes lossless WebP.
        let webp = encoded(colorful(600, 300), ImageFormat::WebP);
        assert!(is_lossless_webp(&webp));
        let out = optimize(Kind::Webp, &webp, &Options::default())
            .unwrap()
            .expect("converted");
        assert_eq!(out.format, Format::Png);
        assert_eq!(decode(&out.bytes).width(), 440);
    }

    #[test]
    fn large_gif_becomes_a_smaller_png() {
        let gif = encoded(colorful(1000, 500), ImageFormat::Gif);
        let out = optimize(Kind::Gif, &gif, &Options::default())
            .unwrap()
            .expect("too big for the screen");
        assert_eq!(out.format, Format::Png);
        assert_eq!(decode(&out.bytes).width(), 440);
    }

    #[test]
    fn small_gif_that_does_not_shrink_is_kept() {
        // One color: GIF's LZW beats PNG at this size. CrossPoint decodes GIF.
        let gif = encoded(
            DynamicImage::ImageRgb8(RgbImage::from_pixel(2, 2, Rgb([0, 0, 0]))),
            ImageFormat::Gif,
        );
        let png_len = encode(
            Format::Png,
            &GrayImage::from_pixel(2, 2, Luma([0])),
            &Options::default(),
        )
        .unwrap()
        .len();
        assert!(
            png_len >= gif.len(),
            "precondition: PNG {png_len} >= GIF {}",
            gif.len()
        );
        assert!(
            optimize(Kind::Gif, &gif, &Options::default())
                .unwrap()
                .is_none()
        );
    }

    /// RIFF/WEBP header followed by chunks given as (fourcc, payload length).
    fn webp_chunks(chunks: &[(&[u8; 4], usize)]) -> Vec<u8> {
        let mut data = b"RIFF\0\0\0\0WEBP".to_vec();
        for (fourcc, len) in chunks {
            data.extend_from_slice(*fourcc);
            data.extend_from_slice(&(*len as u32).to_le_bytes());
            data.extend(std::iter::repeat_n(0, len + len % 2));
        }
        data
    }

    #[test]
    fn tells_lossy_from_lossless_webp() {
        assert!(!is_lossless_webp(&webp_chunks(&[(b"VP8 ", 10)])));
        assert!(is_lossless_webp(&webp_chunks(&[(b"VP8L", 10)])));
        // Extended: VP8X, then an odd-sized chunk (padded), then the image.
        assert!(is_lossless_webp(&webp_chunks(&[
            (b"VP8X", 10),
            (b"ICCP", 3),
            (b"VP8L", 5)
        ])));
        assert!(!is_lossless_webp(&webp_chunks(&[
            (b"VP8X", 10),
            (b"ALPH", 7),
            (b"VP8 ", 5)
        ])));
        // Truncated or empty: treated as lossy.
        assert!(!is_lossless_webp(b"RIFF"));
        assert_eq!(output_format(Kind::Webp, b"RIFF"), Format::Jpeg);
    }
}
