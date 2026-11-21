//! WebAssembly bindings for [prensa], the EPUB optimizer for e-ink readers.
//!
//! Built with `wasm-pack build crates/prensa-wasm --target web` and used by the
//! web page. The whole book is held in memory: the input is read from a
//! `Uint8Array` and the output written to a new one.
//!
//! ```js
//! import init, { optimize } from "./pkg/prensa_wasm.js";
//! await init();
//!
//! const { bytes, report } = optimize(epubBytes, { device: "x3" }, (p) => {
//!   console.log(`${p.done}/${p.total} ${p.entry}`);
//! });
//! ```
//!
//! `optimize` is synchronous and CPU-bound, so call it from a Web Worker to
//! keep the page responsive.

use std::error::Error as StdError;
use std::io::Cursor;

use js_sys::{Array, Function, Object, Reflect, Uint8Array};
use prensa::{Device, Options, Progress, Report, Rotation};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(typescript_custom_section)]
const TS_TYPES: &str = r#"
/** Settings for `optimize`. Every field is optional; omitted means the library default. */
export interface OptimizeOptions {
  /** E-reader to fit images to. Default `"x4"`. */
  device?: "x4" | "x3";
  /** Largest image width in pixels; overrides the device's. */
  maxWidth?: number;
  /** Largest image height in pixels; overrides the device's. */
  maxHeight?: number;
  /** JPEG quality, 1-100. */
  jpegQuality?: number;
  /** Unsharp-mask sigma after downscaling, or `null` to turn sharpening off. */
  sharpen?: number | null;
  /** Turn wide diagrams 90° to fill the screen's height, or `null` (the default) to never turn them. */
  rotate?: "clockwise" | "counterclockwise" | null;
}

/** Where `optimize` is in the book, reported before each entry. */
export interface OptimizeProgress {
  /** Entries already processed: from 0 to `total - 1`. */
  done: number;
  /** Entries in the book (files and directories). */
  total: number;
  /** Path of the entry about to be processed. */
  entry: string;
}

/** A problem that did not stop the book. */
export interface OptimizeWarning {
  /** Path of the entry inside the EPUB. */
  entry: string;
  /** What went wrong and what prensa did about it. */
  message: string;
}

/** What `optimize` did to the book. Byte counts are as stored in the EPUB. */
export interface OptimizeReport {
  fontsRemoved: number;
  fontBytes: number;
  imagesChanged: number;
  imagesConverted: number;
  imagesRotated: number;
  imagesExtracted: number;
  imagesKept: number;
  imagesFailed: number;
  imageBytesBefore: number;
  imageBytesAfter: number;
  warnings: OptimizeWarning[];
}

/** The optimized EPUB and the report. */
export interface OptimizeResult {
  bytes: Uint8Array;
  report: OptimizeReport;
}

/**
 * Optimize an EPUB for e-ink readers.
 *
 * Synchronous and CPU-bound: call it from a Web Worker.
 *
 * @throws {Error} if the options are invalid, the input is not an EPUB, or
 *   `onProgress` throws (its first error is rethrown once the book is done).
 */
export function optimize(
  epub: Uint8Array,
  options?: OptimizeOptions,
  onProgress?: (progress: OptimizeProgress) => void,
): OptimizeResult;
"#;

#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
}

/// Optimize the EPUB in `epub` and return `{ bytes, report }`.
///
/// The TypeScript signature is written by hand in `TS_TYPES`, so the generated
/// one is skipped.
#[wasm_bindgen(skip_typescript)]
pub fn optimize(
    epub: &[u8],
    options: JsValue,
    on_progress: Option<Function>,
) -> Result<JsValue, JsValue> {
    let opts = parse_options(&options)?;

    // The core's progress callback cannot fail, so the first exception thrown
    // by `onProgress` is kept and rethrown once the book is done.
    let mut callback_error = None;
    let mut output = Cursor::new(Vec::new());
    let report = prensa::optimize_with_progress(Cursor::new(epub), &mut output, &opts, |p| {
        if let Some(f) = &on_progress
            && callback_error.is_none()
            && let Err(e) = f.call1(&JsValue::NULL, &progress_to_js(p))
        {
            callback_error = Some(e);
        }
    })
    .map_err(|e| JsError::new(&error_chain(&e)))?;
    if let Some(e) = callback_error {
        // Rethrown as it was thrown, not wrapped in a new Error.
        return Err(e);
    }

    let result = Object::new();
    set(
        &result,
        "bytes",
        &Uint8Array::from(output.get_ref().as_slice()),
    );
    set(&result, "report", &report_to_js(&report));
    Ok(result.into())
}

/// Keys accepted in the options object; anything else is a typo.
const OPTION_KEYS: [&str; 6] = [
    "device",
    "maxWidth",
    "maxHeight",
    "jpegQuality",
    "sharpen",
    "rotate",
];

/// Build [`Options`] from the JS options object.
///
/// `undefined` (or a missing key) keeps the library default; `null` turns
/// `sharpen` and `rotate` off. Ranges are checked by
/// [`OptionsBuilder::build`](prensa::OptionsBuilder::build), not here.
fn parse_options(value: &JsValue) -> Result<Options, JsError> {
    if value.is_undefined() || value.is_null() {
        return Ok(Options::default());
    }
    if !value.is_object() {
        return Err(JsError::new("options must be an object"));
    }
    let obj: &Object = value.unchecked_ref();
    for key in Object::keys(obj) {
        let key = key.as_string().unwrap_or_default();
        if !OPTION_KEYS.contains(&key.as_str()) {
            return Err(JsError::new(&format!("unknown option: {key}")));
        }
    }

    let mut builder = Options::builder();
    let device = match string(obj, "device")?.as_deref() {
        None => Device::default(),
        Some("x4") => Device::X4,
        Some("x3") => Device::X3,
        Some(other) => {
            return Err(JsError::new(&format!(
                "device must be \"x4\" or \"x3\", got \"{other}\""
            )));
        }
    };
    builder = builder.device(device);

    // A side not given keeps the device's.
    let (width, height) = device.max_size();
    let max_width = integer(obj, "maxWidth", u32::MAX.into())?;
    let max_height = integer(obj, "maxHeight", u32::MAX.into())?;
    if max_width.is_some() || max_height.is_some() {
        // Both fit in u32: `integer` checked the bound.
        builder = builder.max_size(
            max_width.map_or(width, |w| w as u32),
            max_height.map_or(height, |h| h as u32),
        );
    }

    if let Some(quality) = integer(obj, "jpegQuality", u8::MAX.into())? {
        builder = builder.jpeg_quality(quality as u8);
    }

    match get(obj, "sharpen")? {
        v if v.is_undefined() => {}
        v if v.is_null() => builder = builder.no_sharpen(),
        v => {
            let sigma = v
                .as_f64()
                .ok_or_else(|| JsError::new("sharpen must be a number or null"))?;
            builder = builder.sharpen(sigma as f32);
        }
    }

    match get(obj, "rotate")? {
        v if v.is_undefined() || v.is_null() => {}
        v => match v.as_string().as_deref() {
            Some("clockwise") => builder = builder.rotate(Rotation::Clockwise),
            Some("counterclockwise") => builder = builder.rotate(Rotation::Counterclockwise),
            _ => {
                return Err(JsError::new(
                    "rotate must be \"clockwise\", \"counterclockwise\" or null",
                ));
            }
        },
    }

    builder.build().map_err(|e| JsError::new(&e.to_string()))
}

fn get(obj: &Object, key: &str) -> Result<JsValue, JsError> {
    // Only fails on a throwing getter or a Proxy trap.
    Reflect::get(obj, &key.into()).map_err(|_| JsError::new(&format!("could not read {key}")))
}

/// The string at `key`, or `None` if it is `undefined`.
fn string(obj: &Object, key: &str) -> Result<Option<String>, JsError> {
    let v = get(obj, key)?;
    if v.is_undefined() {
        return Ok(None);
    }
    v.as_string()
        .map(Some)
        .ok_or_else(|| JsError::new(&format!("{key} must be a string")))
}

/// The integer in `0..=max` at `key`, or `None` if it is `undefined`.
fn integer(obj: &Object, key: &str, max: u64) -> Result<Option<u64>, JsError> {
    let v = get(obj, key)?;
    if v.is_undefined() {
        return Ok(None);
    }
    match v.as_f64() {
        // `max` is at most u32::MAX, so every value in range is exact in f64.
        Some(n) if n.fract() == 0.0 && (0.0..=max as f64).contains(&n) => Ok(Some(n as u64)),
        _ => Err(JsError::new(&format!(
            "{key} must be an integer between 0 and {max}"
        ))),
    }
}

fn set(obj: &Object, key: &str, value: &JsValue) {
    // Cannot fail: `obj` is a plain object we just created.
    let _ = Reflect::set(obj, &key.into(), value);
}

fn progress_to_js(p: Progress<'_>) -> JsValue {
    let obj = Object::new();
    set(&obj, "done", &(p.done as f64).into());
    set(&obj, "total", &(p.total as f64).into());
    set(&obj, "entry", &p.entry.into());
    obj.into()
}

fn report_to_js(r: &Report) -> JsValue {
    let obj = Object::new();
    // Counts and byte sizes become JS numbers: exact up to 2^53, far above
    // any EPUB.
    let fields: [(&str, f64); 10] = [
        ("fontsRemoved", r.fonts_removed as f64),
        ("fontBytes", r.font_bytes as f64),
        ("imagesChanged", r.images_changed as f64),
        ("imagesConverted", r.images_converted as f64),
        ("imagesRotated", r.images_rotated as f64),
        ("imagesExtracted", r.images_extracted as f64),
        ("imagesKept", r.images_kept as f64),
        ("imagesFailed", r.images_failed as f64),
        ("imageBytesBefore", r.image_bytes_before as f64),
        ("imageBytesAfter", r.image_bytes_after as f64),
    ];
    for (key, value) in fields {
        set(&obj, key, &value.into());
    }
    let warnings: Array = r
        .warnings
        .iter()
        .map(|w| {
            let o = Object::new();
            set(&o, "entry", &w.entry.as_str().into());
            set(&o, "message", &w.message.as_str().into());
            JsValue::from(o)
        })
        .collect();
    set(&obj, "warnings", &warnings);
    obj.into()
}

/// `error: source: source…`, so the message says what actually failed
/// ("could not process OEBPS/content.opf: …" rather than only its first part).
fn error_chain(e: &prensa::Error) -> String {
    let mut message = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        message.push_str(": ");
        message.push_str(&s.to_string());
        source = s.source();
    }
    message
}
