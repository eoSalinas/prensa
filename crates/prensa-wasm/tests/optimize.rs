//! Runs in Node: `wasm-pack test --node crates/prensa-wasm`.
#![cfg(target_arch = "wasm32")]

use std::io::{Cursor, Read, Write};

use js_sys::{Array, Function, JSON, Reflect, Uint8Array};
use prensa_wasm::optimize;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::wasm_bindgen_test;
use zip::ZipArchive;
use zip::write::{SimpleFileOptions, ZipWriter};

/// A minimal book: `mimetype` and one 2000x1000 PNG, larger than any device.
fn book() -> Vec<u8> {
    let img = image::RgbImage::from_fn(2000, 1000, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])
    });
    let mut png = Vec::new();
    img.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();

    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("mimetype", &b"application/epub+zip"[..]),
        ("big.png", &png),
    ] {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn options(json: &str) -> JsValue {
    JSON::parse(json).unwrap()
}

fn field(obj: &JsValue, key: &str) -> JsValue {
    Reflect::get(obj, &key.into()).unwrap()
}

fn number(obj: &JsValue, key: &str) -> f64 {
    field(obj, key).as_f64().unwrap()
}

fn error_message(e: JsValue) -> String {
    field(&e, "message").as_string().unwrap()
}

/// Width and height of the PNG at `name` in the output EPUB.
fn png_size(result: &JsValue, name: &str) -> (u32, u32) {
    let bytes = field(result, "bytes")
        .unchecked_into::<Uint8Array>()
        .to_vec();
    let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut data = Vec::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_end(&mut data)
        .unwrap();
    let img = image::load_from_memory(&data).unwrap();
    (img.width(), img.height())
}

#[wasm_bindgen_test]
fn shrinks_images_to_the_device() {
    let result = optimize(&book(), JsValue::UNDEFINED, None).unwrap();

    let report = field(&result, "report");
    assert_eq!(number(&report, "imagesChanged"), 1.0);
    // Sizes are reported; whether they shrink depends on the image (this
    // synthetic gradient deflates to almost nothing).
    assert!(number(&report, "imageBytesBefore") > 0.0);
    assert!(number(&report, "imageBytesAfter") > 0.0);
    assert_eq!(
        field(&report, "warnings")
            .unchecked_into::<Array>()
            .length(),
        0
    );
    // X4 box is 440x733; the 2:1 image is limited by its width.
    assert_eq!(png_size(&result, "big.png"), (440, 220));
}

#[wasm_bindgen_test]
fn a_size_given_alone_keeps_the_other_from_the_device() {
    let result = optimize(&book(), options(r#"{"maxWidth": 100}"#), None).unwrap();
    assert_eq!(png_size(&result, "big.png"), (100, 50));

    // X3 is 484x726: height 200 is the tighter limit.
    let result = optimize(
        &book(),
        options(r#"{"device": "x3", "maxHeight": 200}"#),
        None,
    )
    .unwrap();
    assert_eq!(png_size(&result, "big.png"), (400, 200));
}

#[wasm_bindgen_test]
fn null_turns_sharpening_and_rotation_off() {
    let opts = options(r#"{"sharpen": null, "rotate": null, "jpegQuality": 80}"#);
    assert!(optimize(&book(), opts, None).is_ok());
}

#[wasm_bindgen_test]
fn reports_progress_for_every_entry() {
    let seen = Array::new();
    let push = Function::new_with_args("p", "this.push(p)").bind0(&seen);
    optimize(&book(), JsValue::UNDEFINED, Some(push)).unwrap();

    let entries: Vec<(f64, f64, String)> = seen
        .iter()
        .map(|p| {
            let entry = field(&p, "entry").as_string().unwrap();
            (number(&p, "done"), number(&p, "total"), entry)
        })
        .collect();
    assert_eq!(
        entries,
        [(0.0, 2.0, "mimetype".into()), (1.0, 2.0, "big.png".into())]
    );
}

#[wasm_bindgen_test]
fn rethrows_what_on_progress_throws() {
    let thrower = Function::new_with_args("p", "throw new RangeError('stop at ' + p.entry)");
    let e = optimize(&book(), JsValue::UNDEFINED, Some(thrower)).unwrap_err();
    // The first exception, as thrown.
    assert!(e.is_instance_of::<js_sys::RangeError>());
    assert_eq!(error_message(e), "stop at mimetype");
}

#[wasm_bindgen_test]
fn rejects_invalid_options() {
    let cases = [
        (
            r#"{"jpegQuality": 0}"#,
            "JPEG quality must be between 1 and 100, got 0",
        ),
        (
            r#"{"jpegQuality": 85.5}"#,
            "jpegQuality must be an integer between 0 and 255",
        ),
        (
            r#"{"maxWidth": -1}"#,
            "maxWidth must be an integer between 0 and 4294967295",
        ),
        (
            r#"{"maxWidth": 0}"#,
            "maximum image size must be at least 1x1, got 0x733",
        ),
        (
            r#"{"sharpen": 0}"#,
            "sharpening sigma must be finite and greater than 0, got 0",
        ),
        (r#"{"sharpen": "1"}"#, "sharpen must be a number or null"),
        (
            r#"{"device": "kindle"}"#,
            r#"device must be "x4" or "x3", got "kindle""#,
        ),
        (
            r#"{"rotate": "cw"}"#,
            r#"rotate must be "clockwise", "counterclockwise" or null"#,
        ),
        (r#"{"jpegQuallity": 85}"#, "unknown option: jpegQuallity"),
        ("42", "options must be an object"),
    ];
    for (json, expected) in cases {
        let e = optimize(&book(), options(json), None).unwrap_err();
        assert_eq!(error_message(e), expected, "options {json}");
    }
}

#[wasm_bindgen_test]
fn rejects_input_that_is_not_an_epub() {
    let e = optimize(b"not a zip", JsValue::UNDEFINED, None).unwrap_err();
    let message = error_message(e);
    // The cause from the zip reader follows the summary.
    assert!(
        message.starts_with("input is not a valid EPUB (zip archive): "),
        "{message}"
    );
}
