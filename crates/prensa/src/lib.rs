//! Optimize EPUBs for e-ink readers.
//!
//! The engine works on any reader/writer pair, so it does not care whether the
//! book comes from a file, memory or (later) a browser. Front ends such as the
//! `prensa` CLI handle paths, atomic writes and printing.
//!
//! ```no_run
//! use std::fs::File;
//!
//! let input = File::open("book.epub")?;
//! let output = File::create("book-optimized.epub")?;
//! let report = prensa::optimize(input, output, &prensa::Options::default())?;
//! println!("{} image(s) optimized", report.images_changed);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod error;
mod fonts;
mod images;
mod options;
mod passes;
mod refs;
mod report;

pub use error::{Error, Source};
pub use options::{Device, InvalidOptions, Options, OptionsBuilder};
pub use report::{Progress, Report, Warning};

use error::from_zip;
use images::Optimized;
use passes::{Action, Book, Fallback, Outcome, decide};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// Largest entry read into memory to be rewritten (CSS, OPF, images).
///
/// Real books stay far below this; a bigger entry is a zip bomb or a broken
/// archive, and reading it would exhaust memory. Small in tests so the limit
/// can be hit without writing hundreds of megabytes.
const MAX_ENTRY_SIZE: u64 = if cfg!(test) {
    64 * 1024
} else {
    128 * 1024 * 1024
};

/// Read entry `i` fully, or `None` if it is larger than [`MAX_ENTRY_SIZE`].
///
/// The declared size is checked first, but it comes from the archive and can
/// lie, so the read itself is capped too.
fn read_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    i: usize,
) -> Result<Option<Vec<u8>>, Error> {
    let entry = archive.by_index(i).map_err(from_zip)?;
    if entry.size() > MAX_ENTRY_SIZE {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    entry.take(MAX_ENTRY_SIZE + 1).read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= MAX_ENTRY_SIZE).then_some(bytes))
}

fn too_large() -> String {
    format!(
        "entry is larger than {} MiB",
        MAX_ENTRY_SIZE / (1024 * 1024)
    )
}

/// Copy entry `i` as stored in the input, without decompressing and
/// recompressing it.
fn copy_raw<R, W>(
    archive: &mut ZipArchive<R>,
    writer: &mut ZipWriter<W>,
    i: usize,
) -> Result<(), Error>
where
    R: Read + Seek,
    W: Write + Seek,
{
    let entry = archive.by_index_raw(i).map_err(from_zip)?;
    writer.raw_copy_file(entry).map_err(from_zip)
}

fn malformed(entry: &str, source: Source) -> Error {
    Error::Malformed {
        entry: entry.to_owned(),
        source,
    }
}

/// What became of an image: rewritten (`Some`), kept because rewriting
/// gained nothing (`None`), or not readable as an image (`Err`, a warning).
type ImageResult = anyhow::Result<Option<Optimized>>;

/// Read image entry `i` and run the images pass on it.
fn optimize_image<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    i: usize,
    kind: images::Kind,
    opts: &Options,
) -> Result<ImageResult, Error> {
    Ok(match read_entry(archive, i)? {
        Some(bytes) => images::optimize(kind, &bytes, opts),
        None => Err(anyhow::anyhow!(too_large())),
    })
}

/// A warning for an entry copied unchanged.
fn copied_unchanged(entry: &str, reason: impl std::fmt::Display) -> Warning {
    Warning {
        entry: entry.to_owned(),
        // `{reason}`, not `{reason:#}`: `image` errors already include their
        // cause in the message, so the chain would repeat it. Some decoders
        // end it with a newline; a warning is one line.
        message: format!("{}; copied unchanged", reason.to_string().trim_end()),
    }
}

/// Read an EPUB from `input` and write an optimized copy to `output`.
///
/// Both are taken by value; pass `&mut file` to keep using it afterwards
/// (`&mut T` is `Read`/`Write` + `Seek` whenever `T` is).
///
/// The output follows the EPUB container (OCF) rules: `mimetype` is the first
/// entry and is stored uncompressed. On error, `output` may hold a partial
/// file; callers writing to disk should write to a temporary file and rename it.
pub fn optimize<R, W>(input: R, output: W, opts: &Options) -> Result<Report, Error>
where
    R: Read + Seek,
    W: Write + Seek,
{
    optimize_with_progress(input, output, opts, |_| {})
}

/// Like [`optimize`], calling `on_progress` before each entry is processed.
///
/// On success it is called exactly once per entry (directories included), in
/// archive order, with `done` equal to the entry's index. Nothing is called
/// after the last entry; treat the return of this function as completion.
///
/// The library never prints; front ends use this to draw a progress bar.
/// Images take most of the time, so progress is not linear in entries.
pub fn optimize_with_progress<R, W, F>(
    input: R,
    output: W,
    opts: &Options,
    mut on_progress: F,
) -> Result<Report, Error>
where
    R: Read + Seek,
    W: Write + Seek,
    F: FnMut(Progress<'_>),
{
    let mut archive = ZipArchive::new(input).map_err(from_zip)?;
    let mut writer = ZipWriter::new(output);

    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    writer.start_file("mimetype", stored).map_err(from_zip)?;
    writer.write_all(b"application/epub+zip")?;

    // Name and size of every entry. Sizes in the report are as stored in the
    // EPUB (compressed), so they add up with the file size on disk.
    let entries = (0..archive.len())
        .map(|i| {
            let entry = archive.by_index_raw(i).map_err(from_zip)?;
            let name = entry.name().map_err(from_zip)?.into_owned();
            Ok((name, entry.compressed_size()))
        })
        .collect::<Result<Vec<(String, u64)>, Error>>()?;

    // Fonts declared in the manifest, so a font without a font extension is
    // removed together with its manifest item and `encryption.xml` entry.
    let mut book = Book::default();
    for (i, (name, _)) in entries.iter().enumerate() {
        if !name.to_ascii_lowercase().ends_with(".opf") {
            continue;
        }
        let bytes =
            read_entry(&mut archive, i)?.ok_or_else(|| malformed(name, too_large().into()))?;
        let opf = std::str::from_utf8(&bytes).map_err(|e| malformed(name, Box::new(e)))?;
        book.fonts
            .add_manifest(opf, name)
            .map_err(|e| malformed(name, e.into()))?;
    }

    // Images that may change format (WebP, BMP, GIF) are converted before
    // anything is written: the new file name has to be known by every entry
    // that refers to it, and some of those come earlier in the archive.
    let mut taken: HashSet<String> = entries.iter().map(|(n, _)| n.to_lowercase()).collect();
    let mut converted: HashMap<usize, ImageResult> = HashMap::new();
    for (i, (name, _)) in entries.iter().enumerate() {
        let Some(kind) = images::kind_of(name).filter(|k| k.may_change_format()) else {
            continue;
        };
        let result = optimize_image(&mut archive, i, kind, opts)?;
        if let Ok(Some(image)) = &result {
            let format = image.format;
            book.renames
                .add(name, format.extension(), format.media_type(), &mut taken);
        }
        converted.insert(i, result);
    }

    // Files that survive the passes, used to drop directory entries left empty
    // (e.g. `fonts/` after removing the fonts inside it).
    let kept_files: Vec<&str> = entries
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|n| {
            !n.ends_with('/') && !matches!(decide(n, &book), Action::Skip | Action::RemoveFont)
        })
        .collect();

    let mut report = Report::default();
    let total = entries.len();
    for (i, (name, size)) in entries.iter().enumerate() {
        on_progress(Progress {
            done: i,
            total,
            entry: name,
        });

        if name.ends_with('/') {
            if kept_files.iter().any(|f| f.starts_with(name.as_str())) {
                copy_raw(&mut archive, &mut writer, i)?;
            }
            continue;
        }

        match decide(name, &book) {
            Action::Skip => {}
            Action::RemoveFont => {
                report.fonts_removed += 1;
                report.font_bytes += size;
            }
            Action::CopyRaw => copy_raw(&mut archive, &mut writer, i)?,
            Action::Rewrite(transform, fallback) => {
                let bytes = read_entry(&mut archive, i)?
                    .ok_or_else(|| malformed(name, too_large().into()))?;
                let outcome = match String::from_utf8(bytes) {
                    Ok(text) => transform(&text, name, &book).map_err(Source::from),
                    Err(e) => Err(Box::new(e.utf8_error()) as Source),
                };
                match (outcome, fallback) {
                    (Ok(Outcome::Keep), _) => copy_raw(&mut archive, &mut writer, i)?,
                    (Ok(Outcome::Replace(new_text)), _) => {
                        writer
                            .start_file(name.as_str(), deflated)
                            .map_err(from_zip)?;
                        writer.write_all(new_text.as_bytes())?;
                    }
                    (Ok(Outcome::Drop), _) => {}
                    (Err(_), Fallback::Copy) => copy_raw(&mut archive, &mut writer, i)?,
                    (Err(e), Fallback::CopyWithWarning) => {
                        report.warnings.push(copied_unchanged(
                            name,
                            format_args!("{e}; image references in it not updated"),
                        ));
                        copy_raw(&mut archive, &mut writer, i)?;
                    }
                    // Broken XML aborts everything: no output beats an inconsistent EPUB
                    // (font removed but still listed in the manifest).
                    (Err(e), Fallback::Reject) => return Err(malformed(name, e)),
                }
            }
            Action::Image(kind) => {
                report.image_bytes_before += size;
                let result = match converted.remove(&i) {
                    Some(result) => result,
                    None => optimize_image(&mut archive, i, kind, opts)?,
                };
                match result {
                    Ok(Some(image)) => {
                        report.images_changed += 1;
                        report.image_bytes_after += image.bytes.len() as u64;
                        let new_name = book.renames.new_path(name).unwrap_or(name);
                        if new_name != name {
                            report.images_converted += 1;
                        }
                        // JPEG/PNG are already compressed: deflating them again only costs CPU.
                        writer.start_file(new_name, stored).map_err(from_zip)?;
                        writer.write_all(&image.bytes)?;
                        continue;
                    }
                    // Rewriting gained nothing.
                    Ok(None) => report.images_kept += 1,
                    // An image that fails to decode does not fail the book: it stays as it was.
                    Err(e) => {
                        report.images_failed += 1;
                        report.warnings.push(copied_unchanged(name, e));
                    }
                }
                report.image_bytes_after += size;
                copy_raw(&mut archive, &mut writer, i)?;
            }
        }
    }

    writer.finish().map_err(from_zip)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use quick_xml::Reader;
    use quick_xml::events::Event;
    use std::io::Cursor;

    fn escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    /// Text a reader would show: XHTML text nodes and entity references, in order.
    fn visible_text(xhtml: &str) -> String {
        let mut reader = Reader::from_str(xhtml);
        let mut out = String::new();
        loop {
            match reader.read_event().expect("valid XHTML") {
                Event::Eof => break,
                Event::Text(t) => out.push_str(&t.xml10_content()),
                Event::GeneralRef(r) => out.push_str(&format!("&{};", r.xml10_content())),
                _ => {}
            }
        }
        out
    }

    /// Element structure: start, end and empty tags by name, in order.
    /// Attributes are left out on purpose: prensa may rewrite image
    /// attributes (`src`, `width`), never the tree. Reading positions in sync
    /// services (KOReader's XPath) depend on this tree and on the text.
    fn tags(xhtml: &str) -> Vec<String> {
        let mut reader = Reader::from_str(xhtml);
        let mut out = Vec::new();
        loop {
            let tag = match reader.read_event().expect("valid XHTML") {
                Event::Eof => break,
                Event::Start(e) => format!("<{}>", e.name().as_ref()),
                Event::Empty(e) => format!("<{}/>", e.name().as_ref()),
                Event::End(e) => format!("</{}>", e.name().as_ref()),
                _ => continue,
            };
            out.push(tag);
        }
        out
    }

    /// Build a minimal EPUB in memory: chapters, optional embedded font + @font-face,
    /// optional obfuscation entry. Every chapter shows `images/figure.webp`,
    /// also used as a CSS background; the file exists only if `figure` is given.
    fn build_epub(
        chapters: &[Vec<String>],
        font: Option<&[u8]>,
        figure: Option<&[u8]>,
    ) -> anyhow::Result<Vec<u8>> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file("mimetype", stored)?;
        zip.write_all(b"application/epub+zip")?;
        zip.start_file("META-INF/container.xml", stored)?;
        zip.write_all(br#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#)?;

        let mut manifest =
            String::from(r#"<item href="style.css" id="css" media-type="text/css"/>"#);
        let mut css = String::from("p { margin: 0 }\nh1 { background: url(images/figure.webp) }\n");
        if let Some(bytes) = figure {
            manifest
                .push_str(r#"<item href="images/figure.webp" id="fig" media-type="image/webp"/>"#);
            zip.start_file("OEBPS/images/figure.webp", stored)?;
            zip.write_all(bytes)?;
        }
        if let Some(bytes) = font {
            manifest.push_str(r#"<item href="fonts/f.ttf" id="font" media-type="font/ttf"/>"#);
            css.push_str("@font-face { font-family: \"F\"; src: url(fonts/f.ttf); }\n");
            zip.start_file("OEBPS/fonts/f.ttf", stored)?;
            zip.write_all(bytes)?;
            zip.start_file("META-INF/encryption.xml", stored)?;
            zip.write_all(br#"<encryption><EncryptedData><CipherReference URI="OEBPS/fonts/f.ttf"/></EncryptedData></encryption>"#)?;
        }
        for (i, paragraphs) in chapters.iter().enumerate() {
            manifest.push_str(&format!(
                r#"<item href="ch{i}.xhtml" id="ch{i}" media-type="application/xhtml+xml"/>"#
            ));
            let body: String = paragraphs
                .iter()
                .map(|p| format!("<p>{}</p>", escape(p)))
                .collect();
            zip.start_file(format!("OEBPS/ch{i}.xhtml"), stored)?;
            zip.write_all(format!(r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body><div><h1>Chapter {i}</h1>{body}<img src="images/figure.webp" alt=""/></div></body></html>"#).as_bytes())?;
        }
        zip.start_file("OEBPS/style.css", stored)?;
        zip.write_all(css.as_bytes())?;
        zip.start_file("OEBPS/content.opf", stored)?;
        zip.write_all(format!(r#"<?xml version="1.0"?><package version="3.0"><manifest>{manifest}</manifest></package>"#).as_bytes())?;
        Ok(zip.finish()?.into_inner())
    }

    fn read_all(epub: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut archive = ZipArchive::new(Cursor::new(epub)).unwrap();
        (0..archive.len())
            .map(|i| {
                let mut entry = archive.by_index(i).unwrap();
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                (entry.name().unwrap().into_owned(), bytes)
            })
            .collect()
    }

    /// `f` applied to every chapter, in archive order.
    fn per_chapter<T>(entries: &[(String, Vec<u8>)], f: fn(&str) -> T) -> Vec<T> {
        entries
            .iter()
            .filter(|(name, _)| name.ends_with(".xhtml"))
            .map(|(_, bytes)| f(std::str::from_utf8(bytes).unwrap()))
            .collect()
    }

    /// Zip with one deflated entry per `(name, bytes)`, in order.
    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// The core promise: whatever the book contains, the text a reader sees
        /// and the element structure around it are identical before and after,
        /// and the output is a valid EPUB container.
        #[test]
        fn visible_text_and_structure_are_unchanged(
            chapters in prop::collection::vec(prop::collection::vec("\\PC{0,60}", 0..5), 1..4),
            font in prop::option::of(prop::collection::vec(any::<u8>(), 1..64)),
            has_figure in any::<bool>(),
        ) {
            let figure = has_figure.then(|| webp(8, 8));
            let input = build_epub(&chapters, font.as_deref(), figure.as_deref()).unwrap();
            let mut output = Cursor::new(Vec::new());

            let report = optimize(Cursor::new(&input), &mut output, &Options::default()).unwrap();

            let before = read_all(&input);
            let after = read_all(output.get_ref());
            prop_assert_eq!(per_chapter(&before, visible_text), per_chapter(&after, visible_text));
            prop_assert_eq!(per_chapter(&before, tags), per_chapter(&after, tags));

            // Container rules and the fonts pass, checked on every case.
            prop_assert_eq!(after[0].0.as_str(), "mimetype");
            prop_assert!(after.iter().all(|(name, _)| !fonts::is_font(name)));
            prop_assert!(after.iter().all(|(name, _)| name != "META-INF/encryption.xml"));
            let opf = &after.iter().find(|(n, _)| n.ends_with(".opf")).unwrap().1;
            prop_assert!(!String::from_utf8_lossy(opf).contains("f.ttf"));
            prop_assert_eq!(report.fonts_removed, usize::from(font.is_some()));

            // The images pass: a WebP figure becomes PNG (it is lossless) and
            // every reference follows it.
            if has_figure {
                prop_assert_eq!(report.images_converted, 1);
                prop_assert!(after.iter().all(|(name, _)| !name.ends_with(".webp")));
                prop_assert!(after.iter().any(|(name, _)| name == "OEBPS/images/figure.png"));
                for (name, bytes) in &after {
                    let text = String::from_utf8_lossy(bytes);
                    if name.ends_with(".xhtml") || name.ends_with(".css") || name.ends_with(".opf") {
                        prop_assert!(!text.contains("figure.webp"), "{}", name);
                        prop_assert!(text.contains("images/figure.png"), "{}", name);
                    }
                }
                prop_assert!(String::from_utf8_lossy(opf).contains(r#"media-type="image/png""#));
            }
        }
    }

    /// A lossless WebP (the only kind the `image` crate encodes).
    fn webp(w: u32, h: u32) -> Vec<u8> {
        let img =
            image::RgbImage::from_fn(w, h, |x, y| image::Rgb([x as u8 * 20, y as u8 * 20, 90]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::WebP)
            .unwrap();
        out
    }

    #[test]
    fn structure_ignores_attributes_but_not_elements() {
        let original = r#"<p>A. B.<img src="a.png" width="900"/></p>"#;
        let new_attributes = r#"<p>A. B.<img src="a.jpg"/></p>"#;
        // Same text, one paragraph split in two: what a "split long
        // paragraphs" pass would do, and what this check exists to catch.
        let split = r#"<p>A. </p><p>B.<img src="a.png" width="900"/></p>"#;

        assert_eq!(tags(original), tags(new_attributes));
        assert_eq!(visible_text(original), visible_text(split));
        assert_ne!(tags(original), tags(split));
    }

    #[test]
    fn reports_progress_for_every_entry() {
        let input = build_epub(&[vec!["a".into()], vec!["b".into()]], None, None).unwrap();
        let mut seen = Vec::new();
        let mut output = Cursor::new(Vec::new());
        optimize_with_progress(Cursor::new(&input), &mut output, &Options::default(), |p| {
            seen.push((p.done, p.total, p.entry.to_owned()));
        })
        .unwrap();

        let names: Vec<String> = read_all(&input).into_iter().map(|(n, _)| n).collect();
        let expected: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (i, names.len(), n.clone()))
            .collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn sizes_are_as_stored_in_the_epub() {
        // A deflated font that compresses well: stored size << real size.
        let input = zip_of(&[("f.ttf", &[0; 10_000])]);
        let stored = ZipArchive::new(Cursor::new(&input))
            .unwrap()
            .by_index_raw(0)
            .unwrap()
            .compressed_size();

        let mut output = Cursor::new(Vec::new());
        let report = optimize(Cursor::new(&input), &mut output, &Options::default()).unwrap();
        assert!(stored < 10_000);
        assert_eq!(report.font_bytes, stored);
    }

    #[test]
    fn rejects_non_zip_input() {
        let mut output = Cursor::new(Vec::new());
        let err = optimize(Cursor::new(b"not a zip"), &mut output, &Options::default());
        assert!(matches!(err, Err(Error::InvalidZip(_))));
    }

    fn utf16(text: &str) -> Vec<u8> {
        [0xFF, 0xFE]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect()
    }

    #[test]
    fn rejects_opf_and_encryption_that_are_not_utf8() {
        let opf = utf16(r#"<package><manifest><item href="f.ttf" id="f"/></manifest></package>"#);
        let enc = utf16(
            r#"<encryption><EncryptedData><CipherReference URI="OEBPS/f.ttf"/></EncryptedData></encryption>"#,
        );
        for (name, bytes) in [
            ("OEBPS/content.opf", &opf),
            ("META-INF/encryption.xml", &enc),
        ] {
            let input = zip_of(&[(name, bytes), ("OEBPS/f.ttf", b"font")]);
            let mut output = Cursor::new(Vec::new());
            match optimize(Cursor::new(input), &mut output, &Options::default()) {
                Err(Error::Malformed { entry, .. }) => assert_eq!(entry, name),
                other => panic!("{name}: expected Malformed, got {other:?}"),
            }
        }
    }

    #[test]
    fn unchanged_text_entries_are_copied_as_stored() {
        // No @font-face, no font items: nothing to rewrite. Stored (not
        // deflated) in the input, so a rewrite would show up as deflated.
        let css: &[u8] = b"p { margin: 0 }";
        let opf: &[u8] =
            br#"<package><manifest><item href="ch.xhtml" id="c"/></manifest></package>"#;
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, bytes) in [("OEBPS/style.css", css), ("OEBPS/content.opf", opf)] {
            zip.start_file(name, stored).unwrap();
            zip.write_all(bytes).unwrap();
        }
        let input = zip.finish().unwrap().into_inner();

        let mut output = Cursor::new(Vec::new());
        optimize(Cursor::new(input), &mut output, &Options::default()).unwrap();

        let mut archive = ZipArchive::new(Cursor::new(output.into_inner())).unwrap();
        for (name, bytes) in [("OEBPS/style.css", css), ("OEBPS/content.opf", opf)] {
            let mut entry = archive.by_name(name).unwrap();
            assert_eq!(entry.compression(), CompressionMethod::Stored, "{name}");
            let mut out = Vec::new();
            entry.read_to_end(&mut out).unwrap();
            assert_eq!(out, bytes, "{name}");
        }
    }

    #[test]
    fn copies_css_that_is_not_utf8() {
        // Latin-1 `é` in a comment: invalid UTF-8.
        let css = b"/* caf\xe9 */ @font-face { src: url(f.ttf) }";
        let input = zip_of(&[("OEBPS/style.css", css)]);
        let mut output = Cursor::new(Vec::new());
        optimize(Cursor::new(input), &mut output, &Options::default()).unwrap();
        let after = read_all(output.get_ref());
        let (_, bytes) = after.iter().find(|(n, _)| n == "OEBPS/style.css").unwrap();
        assert_eq!(bytes, css);
    }

    #[test]
    fn read_errors_are_io_errors() {
        /// A reader whose disk fails: not a malformed book.
        struct FailingReader;
        impl Read for FailingReader {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("disk failed"))
            }
        }
        impl Seek for FailingReader {
            fn seek(&mut self, _: std::io::SeekFrom) -> std::io::Result<u64> {
                Ok(100)
            }
        }

        let mut output = Cursor::new(Vec::new());
        let err = optimize(FailingReader, &mut output, &Options::default());
        assert!(matches!(err, Err(Error::Io(_))), "got {err:?}");
    }

    /// Zip with a single entry of `size` zero bytes.
    fn zip_with_zeros(name: &str, size: u64) -> Vec<u8> {
        zip_of(&[(name, &vec![0; size as usize])])
    }

    #[test]
    fn oversized_text_entry_fails_the_book() {
        let input = zip_with_zeros("OEBPS/style.css", MAX_ENTRY_SIZE + 1);
        let mut output = Cursor::new(Vec::new());
        match optimize(Cursor::new(input), &mut output, &Options::default()) {
            Err(Error::Malformed { entry, source }) => {
                assert_eq!(entry, "OEBPS/style.css");
                assert!(source.to_string().contains("larger than"), "{source}");
            }
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn oversized_image_is_copied_unchanged() {
        let input = zip_with_zeros("OEBPS/a.png", MAX_ENTRY_SIZE + 1);
        let mut output = Cursor::new(Vec::new());
        let report = optimize(Cursor::new(&input), &mut output, &Options::default()).unwrap();
        let [warning] = report.warnings.as_slice() else {
            panic!("expected one warning, got {:?}", report.warnings);
        };
        assert_eq!(warning.entry, "OEBPS/a.png");
        assert!(warning.message.contains("larger than"), "{warning}");
        assert_eq!(report.images_failed, 1);
        let after = read_all(output.get_ref());
        assert_eq!(after[1].0, "OEBPS/a.png");
        assert_eq!(after[1].1.len() as u64, MAX_ENTRY_SIZE + 1);
    }

    #[test]
    fn entry_at_the_limit_is_read() {
        let input = zip_with_zeros("OEBPS/style.css", MAX_ENTRY_SIZE);
        let mut archive = ZipArchive::new(Cursor::new(input)).unwrap();
        let bytes = read_entry(&mut archive, 0)
            .unwrap()
            .expect("within the limit");
        assert_eq!(bytes.len() as u64, MAX_ENTRY_SIZE);
    }

    #[test]
    fn removes_fonts_declared_only_by_media_type() {
        let files: [(&str, &[u8]); 4] = [
            (
                "OEBPS/content.opf",
                br#"<package><manifest><item href="fonts/f0001" id="f" media-type="font/ttf"/><item href="ch.xhtml" id="c" media-type="application/xhtml+xml"/></manifest></package>"#,
            ),
            ("OEBPS/fonts/f0001", b"font bytes"),
            ("OEBPS/ch.xhtml", b"<html/>"),
            (
                "META-INF/encryption.xml",
                br#"<encryption><EncryptedData><CipherReference URI="OEBPS/fonts/f0001"/></EncryptedData></encryption>"#,
            ),
        ];
        let input = zip_of(&files);

        let mut output = Cursor::new(Vec::new());
        let report = optimize(Cursor::new(input), &mut output, &Options::default()).unwrap();
        let names: Vec<String> = read_all(output.get_ref())
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, ["mimetype", "OEBPS/content.opf", "OEBPS/ch.xhtml"]);
        assert_eq!(report.fonts_removed, 1);
    }

    #[test]
    fn undecodable_image_is_a_warning_naming_the_entry() {
        let input = zip_of(&[("OEBPS/a.png", b"not a png")]);

        let mut output = Cursor::new(Vec::new());
        let report = optimize(Cursor::new(input), &mut output, &Options::default()).unwrap();
        let [warning] = report.warnings.as_slice() else {
            panic!("expected one warning, got {:?}", report.warnings);
        };
        assert_eq!(warning.entry, "OEBPS/a.png");
        assert!(warning.message.ends_with("; copied unchanged"), "{warning}");
        assert!(warning.to_string().starts_with("OEBPS/a.png: "));
        assert_eq!(
            warning.message.matches("Invalid PNG signature").count(),
            1,
            "{warning}"
        );
        assert_eq!(report.images_failed, 1);
        assert_eq!(report.images_kept, 0);
    }

    #[test]
    fn truncated_jpeg_warning_is_one_line() {
        let input = zip_of(&[("OEBPS/a.jpg", b"\xff\xd8\xff\xe0\x00\x10JFIF\x00")]);
        let mut output = Cursor::new(Vec::new());
        let report = optimize(Cursor::new(input), &mut output, &Options::default()).unwrap();
        let [warning] = report.warnings.as_slice() else {
            panic!("expected one warning, got {:?}", report.warnings);
        };
        assert!(!warning.message.contains('\n'), "{warning:?}");
        assert!(
            warning.message.ends_with("; copied unchanged"),
            "{warning:?}"
        );
    }

    #[test]
    fn broken_opf_names_the_entry() {
        let input = zip_of(&[("OEBPS/content.opf", b"<package><manifest></package>")]);

        let mut output = Cursor::new(Vec::new());
        match optimize(Cursor::new(input), &mut output, &Options::default()) {
            Err(Error::Malformed { entry, .. }) => assert_eq!(entry, "OEBPS/content.opf"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn converted_image_does_not_overwrite_an_existing_file() {
        // `fig.webp` is lossless, so it becomes PNG, but `fig.png` exists.
        let mut png = Vec::new();
        image::DynamicImage::ImageLuma8(image::GrayImage::new(2, 2))
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let figure = webp(4, 4);
        let input = zip_of(&[
            (
                "OEBPS/content.opf",
                br#"<package><manifest><item href="fig.png" id="a" media-type="image/png"/><item href="fig.webp" id="b" media-type="image/webp"/></manifest></package>"#,
            ),
            ("OEBPS/fig.png", &png),
            ("OEBPS/fig.webp", &figure),
            (
                "OEBPS/ch.xhtml",
                br#"<html><body><img src="fig.png"/><img src="fig.webp"/></body></html>"#,
            ),
        ]);

        let mut output = Cursor::new(Vec::new());
        let report = optimize(Cursor::new(input), &mut output, &Options::default()).unwrap();
        let after = read_all(output.get_ref());
        let get = |n: &str| {
            after
                .iter()
                .find(|(name, _)| name == n)
                .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
        };

        assert_eq!(report.images_converted, 1);
        assert!(get("OEBPS/fig.png").is_some());
        assert!(get("OEBPS/fig-1.png").is_some());
        assert!(get("OEBPS/fig.webp").is_none());
        assert_eq!(
            get("OEBPS/ch.xhtml").unwrap(),
            r#"<html><body><img src="fig.png"/><img src="fig-1.png"/></body></html>"#
        );
        let opf = get("OEBPS/content.opf").unwrap();
        assert!(
            opf.contains(r#"<item href="fig.png" id="a" media-type="image/png"/>"#),
            "{opf}"
        );
        assert!(
            opf.contains(r#"<item href="fig-1.png" id="b" media-type="image/png"/>"#),
            "{opf}"
        );
    }

    #[test]
    fn undecodable_webp_keeps_its_name_and_references() {
        let xhtml: &[u8] = br#"<html><body><img src="a.webp"/></body></html>"#;
        let input = zip_of(&[("OEBPS/a.webp", b"not a webp"), ("OEBPS/ch.xhtml", xhtml)]);
        let mut output = Cursor::new(Vec::new());
        let report = optimize(Cursor::new(input), &mut output, &Options::default()).unwrap();
        let after = read_all(output.get_ref());
        assert_eq!(report.images_failed, 1);
        assert_eq!(report.images_converted, 0);
        assert!(after.iter().any(|(n, _)| n == "OEBPS/a.webp"));
        assert!(
            after
                .iter()
                .any(|(n, b)| n == "OEBPS/ch.xhtml" && b == xhtml)
        );
    }

    #[test]
    fn broken_chapter_is_copied_with_a_warning() {
        // An unterminated tag: not even attributes can be read.
        let broken: &[u8] = br#"<html><body><p><img src="a.webp"</p></body></html>"#;
        let figure = webp(4, 4);
        let input = zip_of(&[("OEBPS/a.webp", &figure), ("OEBPS/ch.xhtml", broken)]);
        let mut output = Cursor::new(Vec::new());
        let report = optimize(Cursor::new(input), &mut output, &Options::default()).unwrap();
        let after = read_all(output.get_ref());

        assert!(
            after
                .iter()
                .any(|(n, b)| n == "OEBPS/ch.xhtml" && b == broken)
        );
        let [warning] = report.warnings.as_slice() else {
            panic!("expected one warning, got {:?}", report.warnings);
        };
        assert_eq!(warning.entry, "OEBPS/ch.xhtml");
        assert!(
            warning
                .message
                .ends_with("image references in it not updated; copied unchanged"),
            "{warning}"
        );
    }
}
