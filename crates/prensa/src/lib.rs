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

pub use error::{Error, Source};
pub use options::{InvalidOptions, Options, OptionsBuilder};

use error::from_zip;
use fonts::Fonts;
use std::io::{Read, Seek, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// What [`optimize`] did to the book.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct Report {
    /// Embedded font files removed.
    pub fonts_removed: usize,
    /// Size of the removed fonts, in bytes (compressed, as stored in the EPUB).
    pub font_bytes: u64,
    /// Images rewritten.
    pub images_changed: usize,
    /// Images kept as they were because rewriting gained nothing.
    pub images_kept: usize,
    /// Images copied unchanged because they could not be rewritten (failed to
    /// decode, or too large to read). Each one also has a warning.
    pub images_failed: usize,
    /// Size of all images before, in bytes (compressed, as stored in the EPUB).
    pub image_bytes_before: u64,
    /// Size of all images after, in bytes (as stored in the EPUB).
    pub image_bytes_after: u64,
    /// Problems that did not stop the book, e.g. an image that failed to
    /// decode and was copied unchanged. One per problem.
    pub warnings: Vec<Warning>,
}

/// A problem that did not stop the book. `Display` gives `entry: message`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Warning {
    /// Path of the entry inside the EPUB.
    pub entry: String,
    /// What went wrong and what prensa did about it, e.g. "failed to decode;
    /// copied unchanged".
    pub message: String,
}

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.entry, self.message)
    }
}

/// Where [`optimize_with_progress`] is in the book.
///
/// Reported before each entry, so `done` goes from `0` to `total - 1`; there is
/// no call with `done == total`. The book is finished when
/// [`optimize_with_progress`] returns.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct Progress<'a> {
    /// Entries already processed.
    pub done: usize,
    /// Entries in the book (files and directories).
    pub total: usize,
    /// Path of the entry about to be processed.
    pub entry: &'a str,
}

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

/// What to do with each zip entry.
enum Action {
    /// Already written (`mimetype`, written first).
    Skip,
    /// Dropped and counted in the report.
    RemoveFont,
    CopyRaw,
    /// `Ok(None)` means the entry is dropped from the output.
    Rewrite(fn(&str, &Fonts) -> anyhow::Result<Option<String>>, NotUtf8),
    Image(images::Kind),
}

/// What to do with a text entry that is not valid UTF-8.
enum NotUtf8 {
    /// Copy it unchanged. Fine when skipping the rewrite leaves the book
    /// consistent: a leftover `@font-face` points to a missing file, which
    /// readers already handle by falling back to their own fonts.
    CopyRaw,
    /// Fail the book. Copying would list removed fonts in the manifest or in
    /// `encryption.xml`.
    Reject,
}

fn decide(name: &str, fonts: &Fonts) -> Action {
    let lower = name.to_ascii_lowercase();
    if name == "mimetype" {
        Action::Skip
    } else if fonts.contains(name) {
        Action::RemoveFont
    } else if lower.ends_with(".css") {
        Action::Rewrite(fonts::strip_font_faces, NotUtf8::CopyRaw)
    } else if lower.ends_with(".opf") {
        Action::Rewrite(fonts::remove_manifest_items, NotUtf8::Reject)
    } else if lower == "meta-inf/encryption.xml" {
        Action::Rewrite(fonts::clean_encryption, NotUtf8::Reject)
    } else if let Some(kind) = images::kind_of(name) {
        Action::Image(kind)
    } else {
        Action::CopyRaw
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
    let mut fonts = Fonts::default();
    for (i, (name, _)) in entries.iter().enumerate() {
        if !name.to_ascii_lowercase().ends_with(".opf") {
            continue;
        }
        let bytes =
            read_entry(&mut archive, i)?.ok_or_else(|| malformed(name, too_large().into()))?;
        let opf = std::str::from_utf8(&bytes).map_err(|e| malformed(name, Box::new(e)))?;
        fonts
            .add_manifest(opf, name)
            .map_err(|e| malformed(name, e.into()))?;
    }

    // Files that survive the passes, used to drop directory entries left empty
    // (e.g. `fonts/` after removing the fonts inside it).
    let kept_files: Vec<&str> = entries
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|n| {
            !n.ends_with('/') && !matches!(decide(n, &fonts), Action::Skip | Action::RemoveFont)
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

        match decide(name, &fonts) {
            Action::Skip => {}
            Action::RemoveFont => {
                report.fonts_removed += 1;
                report.font_bytes += size;
            }
            Action::CopyRaw => copy_raw(&mut archive, &mut writer, i)?,
            Action::Rewrite(transform, not_utf8) => {
                let bytes = read_entry(&mut archive, i)?
                    .ok_or_else(|| malformed(name, too_large().into()))?;
                match String::from_utf8(bytes) {
                    Ok(text) => {
                        // Broken XML aborts everything: no output beats an inconsistent EPUB
                        // (font removed but still listed in the manifest).
                        let result =
                            transform(&text, &fonts).map_err(|e| malformed(name, e.into()))?;
                        if let Some(new_text) = result {
                            writer
                                .start_file(name.as_str(), deflated)
                                .map_err(from_zip)?;
                            writer.write_all(new_text.as_bytes())?;
                        }
                    }
                    Err(e) => match not_utf8 {
                        NotUtf8::CopyRaw => copy_raw(&mut archive, &mut writer, i)?,
                        NotUtf8::Reject => return Err(malformed(name, Box::new(e.utf8_error()))),
                    },
                }
            }
            Action::Image(kind) => {
                report.image_bytes_before += size;
                let result = match read_entry(&mut archive, i)? {
                    Some(bytes) => images::optimize(kind, &bytes, opts),
                    None => Err(anyhow::anyhow!(too_large())),
                };
                match result {
                    Ok(Some(new_bytes)) => {
                        report.images_changed += 1;
                        report.image_bytes_after += new_bytes.len() as u64;
                        // JPEG/PNG are already compressed: deflating them again only costs CPU.
                        writer.start_file(name.as_str(), stored).map_err(from_zip)?;
                        writer.write_all(&new_bytes)?;
                        continue;
                    }
                    // Rewriting gained nothing.
                    Ok(None) => report.images_kept += 1,
                    // An image that fails to decode does not fail the book: it stays as it was.
                    Err(e) => {
                        report.images_failed += 1;
                        report.warnings.push(Warning {
                            entry: name.clone(),
                            // `{e}`, not `{e:#}`: `image` errors already include their
                            // cause in the message, so the chain would repeat it. Some
                            // decoders end it with a newline; a warning is one line.
                            message: format!("{}; copied unchanged", e.to_string().trim_end()),
                        });
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

    /// Build a minimal EPUB in memory: chapters, optional embedded font + @font-face,
    /// optional obfuscation entry.
    fn build_epub(chapters: &[Vec<String>], font: Option<&[u8]>) -> anyhow::Result<Vec<u8>> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file("mimetype", stored)?;
        zip.write_all(b"application/epub+zip")?;
        zip.start_file("META-INF/container.xml", stored)?;
        zip.write_all(br#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#)?;

        let mut manifest =
            String::from(r#"<item href="style.css" id="css" media-type="text/css"/>"#);
        let mut css = String::from("p { margin: 0 }\n");
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
            zip.write_all(format!(r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body>{body}</body></html>"#).as_bytes())?;
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

    fn chapter_texts(entries: &[(String, Vec<u8>)]) -> Vec<String> {
        entries
            .iter()
            .filter(|(name, _)| name.ends_with(".xhtml"))
            .map(|(_, bytes)| visible_text(std::str::from_utf8(bytes).unwrap()))
            .collect()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// The core promise: whatever the book contains, the text a reader sees
        /// is identical before and after, and the output is a valid EPUB container.
        #[test]
        fn visible_text_is_unchanged(
            chapters in prop::collection::vec(prop::collection::vec("\\PC{0,60}", 0..5), 1..4),
            font in prop::option::of(prop::collection::vec(any::<u8>(), 1..64)),
        ) {
            let input = build_epub(&chapters, font.as_deref()).unwrap();
            let mut output = Cursor::new(Vec::new());

            let report = optimize(Cursor::new(&input), &mut output, &Options::default()).unwrap();

            let before = read_all(&input);
            let after = read_all(output.get_ref());
            prop_assert_eq!(chapter_texts(&before), chapter_texts(&after));

            // Container rules and the fonts pass, checked on every case.
            prop_assert_eq!(after[0].0.as_str(), "mimetype");
            prop_assert!(after.iter().all(|(name, _)| !fonts::is_font(name)));
            prop_assert!(after.iter().all(|(name, _)| name != "META-INF/encryption.xml"));
            let opf = &after.iter().find(|(n, _)| n.ends_with(".opf")).unwrap().1;
            prop_assert!(!String::from_utf8_lossy(opf).contains("f.ttf"));
            prop_assert_eq!(report.fonts_removed, usize::from(font.is_some()));
        }
    }

    #[test]
    fn reports_progress_for_every_entry() {
        let input = build_epub(&[vec!["a".into()], vec!["b".into()]], None).unwrap();
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
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("f.ttf", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&[0u8; 10_000]).unwrap();
        let input = zip.finish().unwrap().into_inner();
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

    /// Zip with one deflated entry per `(name, bytes)`, plus a font.
    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries.iter().chain([&("OEBPS/f.ttf", &b"font"[..])]) {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
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
            let input = zip_with(&[(name, bytes)]);
            let mut output = Cursor::new(Vec::new());
            match optimize(Cursor::new(input), &mut output, &Options::default()) {
                Err(Error::Malformed { entry, .. }) => assert_eq!(entry, name),
                other => panic!("{name}: expected Malformed, got {other:?}"),
            }
        }
    }

    #[test]
    fn copies_css_that_is_not_utf8() {
        // Latin-1 `é` in a comment: invalid UTF-8.
        let css = b"/* caf\xe9 */ @font-face { src: url(f.ttf) }";
        let input = zip_with(&[("OEBPS/style.css", css)]);
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

    /// Zip with a single deflated entry of `size` zero bytes.
    fn zip_with_zeros(name: &str, size: u64) -> Vec<u8> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        std::io::copy(&mut std::io::repeat(0).take(size), &mut zip).unwrap();
        zip.finish().unwrap().into_inner()
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
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
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
        for (name, bytes) in files {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        let input = zip.finish().unwrap().into_inner();

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
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("OEBPS/a.png", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"not a png").unwrap();
        let input = zip.finish().unwrap().into_inner();

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
        let input = zip_with(&[("OEBPS/a.jpg", b"\xff\xd8\xff\xe0\x00\x10JFIF\x00")]);
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
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("OEBPS/content.opf", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"<package><manifest></package>").unwrap();
        let input = zip.finish().unwrap().into_inner();

        let mut output = Cursor::new(Vec::new());
        match optimize(Cursor::new(input), &mut output, &Options::default()) {
            Err(Error::Malformed { entry, .. }) => assert_eq!(entry, "OEBPS/content.opf"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }
}
