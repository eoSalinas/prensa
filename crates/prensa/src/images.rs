//! `images` pass: fit each image to the e-reader screen.
//!
//! - downscale to fit the configured box (aspect ratio kept; never upscales);
//! - convert to grayscale (e-ink shows no color), flattening transparency onto white;
//! - JPEG output is always baseline: CrossPoint decodes progressive JPEG at 1/8
//!   resolution, so the image comes out blurry;
//! - formats CrossPoint cannot decode (WebP, BMP) become JPEG or PNG, and GIF
//!   becomes PNG when that is smaller or the image must shrink. The caller
//!   renames the file, since the reader picks a decoder from the extension;
//! - optionally, wide images that would be shrunk to less than a third are
//!   turned 90° when that shows them at least 30% larger ([`rotation_for`]);
//!   the caller drops their `width`/`height` in chapters;
//! - never makes things worse: if the result is larger and the image did not
//!   *need* to change, the original is kept.

use anyhow::Result;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, GrayImage, ImageEncoder, ImageFormat};

use crate::{Options, Rotation};

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
    /// Usual file extension and media type of the format.
    pub(crate) fn extension_and_media_type(self) -> (&'static str, &'static str) {
        match self {
            Kind::Jpeg => ("jpg", "image/jpeg"),
            Kind::Png => ("png", "image/png"),
            Kind::Gif => ("gif", "image/gif"),
            Kind::Bmp => ("bmp", "image/bmp"),
            Kind::Webp => ("webp", "image/webp"),
        }
    }

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

/// Width and height from the image header, without decoding the pixels.
/// `None` if the header cannot be read; decoding will report why.
pub(crate) fn dimensions(kind: Kind, data: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::with_format(std::io::Cursor::new(data), kind.image_format())
        .into_dimensions()
        .ok()
}

/// Only images shrunk below this scale upright are turned: below a third,
/// small text in a diagram is no longer readable. Each turned image makes the
/// reader turn the device, so images that are merely reduced stay upright
/// (in Mastering Bitcoin, 600 px diagrams fitted to 440 px read fine).
const MAX_UPRIGHT_SCALE: f64 = 1.0 / 3.0;

/// Turning an image must show it at least this much larger to be worth
/// making the reader turn the device.
const MIN_ROTATION_GAIN: f64 = 1.3;

/// Direction to turn an image of `width` x `height`, or `None` to leave it.
///
/// Only images shrunk below [`MAX_UPRIGHT_SCALE`] upright are turned, and only
/// when they are then shown at least [`MIN_ROTATION_GAIN`] times larger. With
/// the 440x733 box, that is a landscape image wider than 1320 px with an
/// aspect ratio of about 1.3 or more; the gain is capped at 733/440 = 1.67.
pub(crate) fn rotation_for(width: u32, height: u32, opts: &Options) -> Option<Rotation> {
    let rotation = opts.rotate()?;
    let (box_w, box_h) = (f64::from(opts.max_width()), f64::from(opts.max_height()));
    let (w, h) = (f64::from(width), f64::from(height));
    let upright = (box_w / w).min(box_h / h).min(1.0);
    if upright >= MAX_UPRIGHT_SCALE {
        return None;
    }
    let turned = (box_w / h).min(box_h / w).min(1.0);
    (turned >= upright * MIN_ROTATION_GAIN).then_some(rotation)
}

/// Grayscale with transparency flattened onto white.
///
/// Runs once per pixel of every image with alpha, so it works on the raw
/// buffer in integer math: `f32::round` is a libm call on x86-64.
fn flatten_alpha(img: &DynamicImage) -> GrayImage {
    let owned;
    let rgba = match img {
        DynamicImage::ImageRgba8(rgba) => rgba,
        _ => {
            owned = img.to_rgba8();
            &owned
        }
    };
    let gray = rgba
        .as_raw()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&p| flatten_pixel(p))
        .collect();
    GrayImage::from_raw(rgba.width(), rgba.height(), gray).expect("one gray byte per pixel")
}

/// Rec. 601 luma of an RGBA pixel blended onto white by its alpha, rounded.
fn flatten_pixel([r, g, b, a]: [u8; 4]) -> u8 {
    let [r, g, b, a] = [r, g, b, a].map(u32::from);
    // Luma in thousandths (0..=255_000), so the weights stay exact.
    let luma = 299 * r + 587 * g + 114 * b;
    // luma * a/255 + 255 * (1 - a/255), over 255 * 1000, rounded half up.
    // At most 255_000 * 255 + 127_500, well within u32.
    ((luma * a + 255_000 * (255 - a) + 127_500) / 255_000) as u8
}

/// Grayscale with alpha flattened onto white, downscaled to fit the box.
fn to_screen_gray(img: &DynamicImage, rotation: Option<Rotation>, opts: &Options) -> GrayImage {
    let gray = if img.color().has_alpha() {
        flatten_alpha(img)
    } else {
        img.to_luma8()
    };

    let gray = match rotation {
        None => gray,
        Some(Rotation::Clockwise) => image::imageops::rotate90(&gray),
        Some(Rotation::Counterclockwise) => image::imageops::rotate270(&gray),
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
    /// Turned 90°: references to it must lose sizes set for the original.
    pub(crate) rotated: bool,
}

/// Returns `Ok(None)` when the original should be kept as-is.
pub(crate) fn optimize(kind: Kind, data: &[u8], opts: &Options) -> Result<Option<Optimized>> {
    let img = image::load_from_memory_with_format(data, kind.image_format())?;
    let format = output_format(kind, data);

    // Cases where the change is required even if the file grows.
    let too_big = img.width() > opts.max_width() || img.height() > opts.max_height();
    let progressive = kind == Kind::Jpeg && is_progressive_jpeg(data);
    let rotation = rotation_for(img.width(), img.height(), opts);
    let must_change = too_big || progressive || !kind.device_decodes() || rotation.is_some();

    let bytes = encode(format, &to_screen_gray(&img, rotation, opts), opts)?;
    if bytes.len() >= data.len() && !must_change {
        return Ok(None);
    }
    Ok(Some(Optimized {
        bytes,
        format,
        rotated: rotation.is_some(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Luma, LumaA, Rgb, RgbImage, Rgba, RgbaImage};
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

    fn no_rotate() -> Options {
        Options::builder().no_rotate().build().unwrap()
    }

    #[test]
    fn downscales_to_fit_keeping_aspect_ratio() {
        let out = optimize(Kind::Jpeg, &colorful_jpeg(2000, 1000), &no_rotate())
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
    fn turns_wide_images_that_gain_enough() {
        let opts = Options::builder()
            .rotate(Rotation::Clockwise)
            .build()
            .unwrap(); // 440x733
        let cw = Some(Rotation::Clockwise);
        // 2391x1000 (Pro Git, Figure 10): 18% upright, 1.67x larger turned.
        assert_eq!(rotation_for(2391, 1000, &opts), cw);
        // Shrunk to 44%: still readable upright.
        assert_eq!(rotation_for(1000, 500, &opts), None);
        // 600 px wide: reduced to 73%, stays upright.
        assert_eq!(rotation_for(600, 331, &opts), None);
        // Fits the box.
        assert_eq!(rotation_for(440, 200, &opts), None);
        // Portrait and square: turning makes them smaller or no larger.
        assert_eq!(rotation_for(1000, 4000, &opts), None);
        assert_eq!(rotation_for(3000, 3000, &opts), None);
        // Shrunk to 31% but only 1.2:1: gains 1.2x, below the threshold.
        assert_eq!(rotation_for(1400, 1170, &opts), None);
        // Off by default.
        assert_eq!(rotation_for(2391, 1000, &Options::default()), None);
    }

    fn rotate_cw() -> Options {
        Options::builder()
            .rotate(Rotation::Clockwise)
            .build()
            .unwrap()
    }

    #[test]
    fn turned_image_fills_the_height() {
        let out = optimize(Kind::Jpeg, &colorful_jpeg(2000, 1000), &rotate_cw())
            .unwrap()
            .unwrap();
        assert!(out.rotated);
        let img = decode(&out.bytes);
        // 1000x2000 after turning, fitted to 440x733.
        assert_eq!((img.width(), img.height()), (367, 733));
        // Off by default: fitted upright.
        let upright = optimize(Kind::Jpeg, &colorful_jpeg(2000, 1000), &Options::default())
            .unwrap()
            .unwrap();
        assert!(!upright.rotated);
    }

    #[test]
    fn turns_in_the_chosen_direction() {
        // Black left half: clockwise puts it on top, counterclockwise at the bottom.
        let img = DynamicImage::ImageRgb8(RgbImage::from_fn(2000, 1000, |x, _| {
            if x < 1000 {
                Rgb([0, 0, 0])
            } else {
                Rgb([255, 255, 255])
            }
        }));
        let png = encoded(img, ImageFormat::Png);
        let top_and_bottom = |rotation| {
            let opts = Options::builder().rotate(rotation).build().unwrap();
            let out = decode(&optimize(Kind::Png, &png, &opts).unwrap().unwrap().bytes).to_luma8();
            (
                out.get_pixel(out.width() / 2, 5).0[0],
                out.get_pixel(out.width() / 2, out.height() - 5).0[0],
            )
        };
        assert_eq!(top_and_bottom(Rotation::Clockwise), (0, 255));
        assert_eq!(top_and_bottom(Rotation::Counterclockwise), (255, 0));
    }

    #[test]
    fn never_upscales() {
        // Tested below `optimize`, which may keep a small image untouched and
        // then has no output to check.
        let small = decode(&colorful_jpeg(100, 50));
        let out = to_screen_gray(&small, None, &Options::default());
        assert_eq!(out.dimensions(), (100, 50));
    }

    #[test]
    fn fits_a_tall_image_by_height() {
        let tall = decode(&colorful_jpeg(500, 2000));
        let out = to_screen_gray(&tall, None, &Options::default());
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
    fn flattening_matches_the_float_formula() {
        // The formula this replaced, in f32 as it was written.
        let float = |[r, g, b, a]: [u8; 4]| {
            let luma = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
            let alpha = a as f32 / 255.0;
            (luma * alpha + 255.0 * (1.0 - alpha)).round() as u8
        };
        let steps = (0..=255).step_by(15).chain([1, 128, 254]);
        for r in steps.clone() {
            for g in steps.clone() {
                for b in steps.clone() {
                    for a in 0..=255 {
                        let px = [r, g, b, a];
                        assert!(
                            flatten_pixel(px).abs_diff(float(px)) <= 1,
                            "{px:?}: {} vs {}",
                            flatten_pixel(px),
                            float(px)
                        );
                    }
                }
            }
        }
        assert_eq!(flatten_pixel([0, 0, 0, 0]), 255);
        assert_eq!(flatten_pixel([0, 0, 0, 255]), 0);
        assert_eq!(flatten_pixel([255, 255, 255, 255]), 255);
    }

    #[test]
    fn flattens_gray_with_alpha() {
        // Not RGBA8, so it goes through the conversion.
        let img = DynamicImage::ImageLumaA8(image::ImageBuffer::from_fn(2, 1, |x, _| {
            LumaA([0, if x == 0 { 0 } else { 255 }])
        }));
        let gray = flatten_alpha(&img);
        assert_eq!(gray.as_raw(), &[255, 0]);
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
        let out = optimize(Kind::Webp, &webp, &no_rotate())
            .unwrap()
            .expect("converted");
        assert_eq!(out.format, Format::Png);
        assert_eq!(decode(&out.bytes).width(), 440);
    }

    #[test]
    fn large_gif_becomes_a_smaller_png() {
        let gif = encoded(colorful(1000, 500), ImageFormat::Gif);
        let out = optimize(Kind::Gif, &gif, &no_rotate())
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
