mod fonts;
mod images;

use anyhow::{Context, Result, bail};
use clap::Parser;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// Optimize EPUBs for e-ink readers. Writes a copy; the input is never modified.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Input EPUB
    input: PathBuf,
    /// Output EPUB
    output: PathBuf,
    /// Maximum image width, in pixels
    #[arg(long, default_value_t = images::DEFAULT_MAX_WIDTH)]
    max_width: u32,
    /// Maximum image height, in pixels
    #[arg(long, default_value_t = images::DEFAULT_MAX_HEIGHT)]
    max_height: u32,
    /// JPEG quality (1-100)
    #[arg(long, default_value_t = images::DEFAULT_JPEG_QUALITY, value_parser = clap::value_parser!(u8).range(1..=100))]
    quality: u8,
    /// Sharpening after downscaling (unsharp mask sigma; 0 disables)
    #[arg(long, default_value_t = images::DEFAULT_SHARPEN)]
    sharpen: f32,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = images::Config {
        max_width: cli.max_width,
        max_height: cli.max_height,
        jpeg_quality: cli.quality,
        sharpen: cli.sharpen,
    };
    let stats = optimize(&cli.input, &cli.output, &cfg)?;
    let mb = |b: u64| b as f64 / 1e6;
    println!("fonts removed: {} file(s), {:.1} MB", stats.fonts_removed, mb(stats.font_bytes));
    println!(
        "images: {} optimized, {} kept, {:.1} -> {:.1} MB",
        stats.images_changed,
        stats.images_kept,
        mb(stats.image_bytes_before),
        mb(stats.image_bytes_after)
    );
    if stats.image_errors > 0 {
        println!("warning: {} image(s) failed to decode and were copied unchanged", stats.image_errors);
    }
    Ok(())
}

#[derive(Default)]
struct Stats {
    fonts_removed: usize,
    font_bytes: u64,
    images_changed: usize,
    images_kept: usize,
    image_errors: usize,
    image_bytes_before: u64,
    image_bytes_after: u64,
}

/// What to do with each zip entry.
enum Action {
    Skip,
    CopyRaw,
    /// `Ok(None)` means the entry is dropped from the output.
    Rewrite(fn(&str) -> Result<Option<String>>),
    Image(images::Kind),
}

fn decide(name: &str) -> Action {
    let lower = name.to_ascii_lowercase();
    if name == "mimetype" || fonts::is_font(name) {
        Action::Skip
    } else if lower.ends_with(".css") {
        Action::Rewrite(fonts::strip_font_faces)
    } else if lower.ends_with(".opf") {
        Action::Rewrite(fonts::remove_manifest_items)
    } else if lower == "meta-inf/encryption.xml" {
        Action::Rewrite(fonts::clean_encryption)
    } else if let Some(kind) = images::kind_of(name) {
        Action::Image(kind)
    } else {
        Action::CopyRaw
    }
}

/// Read an EPUB and write an optimized copy. The input is never modified.
///
/// Enforces the two EPUB container (OCF) rules:
/// `mimetype` is the first entry and is stored uncompressed.
fn optimize(input: &Path, output: &Path, cfg: &images::Config) -> Result<Stats> {
    let input_real = input
        .canonicalize()
        .with_context(|| format!("opening {}", input.display()))?;
    if input_real == output.canonicalize().unwrap_or_default() {
        bail!("output must not be the input file");
    }

    let file = File::open(input).with_context(|| format!("opening {}", input.display()))?;
    let mut archive = ZipArchive::new(file).context("input is not a valid zip")?;

    // Atomic write: write to a temp file in the same directory and rename at the end.
    // If anything fails midway, the temp file is deleted and `output` is never half-written.
    let dir = output.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(dir)?;
    let mut writer = ZipWriter::new(tmp);

    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    writer.start_file("mimetype", stored)?;
    writer.write_all(b"application/epub+zip")?;

    // Files that survive the passes, used to drop directory entries left empty
    // (e.g. `fonts/` after removing the fonts inside it).
    let names = (0..archive.len())
        .map(|i| Ok(archive.by_index_raw(i)?.name()?.into_owned()))
        .collect::<Result<Vec<String>>>()?;
    let kept_files: Vec<&str> = names
        .iter()
        .filter(|n| !n.ends_with('/') && !matches!(decide(n), Action::Skip))
        .map(String::as_str)
        .collect();

    let mut stats = Stats::default();
    for i in 0..archive.len() {
        // `by_index_raw` borrows `archive` mutably: one entry at a time.
        // `into_owned` copies the name so the borrow ends here.
        let (name, size) = {
            let entry = archive.by_index_raw(i)?;
            (entry.name()?.into_owned(), entry.size())
        };

        if name.ends_with('/') {
            if kept_files.iter().any(|f| f.starts_with(name.as_str())) {
                writer.raw_copy_file(archive.by_index_raw(i)?)?;
            }
            continue;
        }

        match decide(&name) {
            Action::Skip => {
                if fonts::is_font(&name) {
                    stats.fonts_removed += 1;
                    stats.font_bytes += size;
                }
            }
            // Copy the compressed bytes as-is, without decompressing and recompressing.
            Action::CopyRaw => writer.raw_copy_file(archive.by_index_raw(i)?)?,
            Action::Rewrite(transform) => {
                let mut bytes = Vec::new();
                archive.by_index(i)?.read_to_end(&mut bytes)?;
                match String::from_utf8(bytes) {
                    Ok(text) => {
                        // Broken XML aborts everything: no output beats an inconsistent EPUB
                        // (font removed but still listed in the manifest).
                        let result = transform(&text).with_context(|| format!("processing {name}"))?;
                        if let Some(new_text) = result {
                            writer.start_file(name.as_str(), deflated)?;
                            writer.write_all(new_text.as_bytes())?;
                        }
                    }
                    // Not UTF-8: do not risk it, copy unchanged.
                    Err(_) => writer.raw_copy_file(archive.by_index_raw(i)?)?,
                }
            }
            Action::Image(kind) => {
                let mut bytes = Vec::new();
                archive.by_index(i)?.read_to_end(&mut bytes)?;
                stats.image_bytes_before += size;
                match images::optimize(kind, &bytes, cfg) {
                    Ok(Some(new_bytes)) => {
                        stats.images_changed += 1;
                        stats.image_bytes_after += new_bytes.len() as u64;
                        // JPEG/PNG are already compressed: deflating them again only costs CPU.
                        writer.start_file(name.as_str(), stored)?;
                        writer.write_all(&new_bytes)?;
                    }
                    // An image that fails to decode does not fail the book: it stays as it was.
                    result => {
                        if let Err(e) = result {
                            eprintln!("warning: {name}: {e:#}");
                            stats.image_errors += 1;
                        } else {
                            stats.images_kept += 1;
                        }
                        stats.image_bytes_after += size;
                        writer.raw_copy_file(archive.by_index_raw(i)?)?;
                    }
                }
            }
        }
    }

    let tmp = writer.finish()?;
    tmp.persist(output)
        .with_context(|| format!("writing {}", output.display()))?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use quick_xml::Reader;
    use quick_xml::events::Event;

    fn escape(text: &str) -> String {
        text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
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

    /// Build a minimal EPUB: chapters, optional embedded font + @font-face, optional obfuscation entry.
    fn build_epub(path: &Path, chapters: &[Vec<String>], font: Option<&[u8]>) -> Result<()> {
        let mut zip = ZipWriter::new(File::create(path)?);
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file("mimetype", stored)?;
        zip.write_all(b"application/epub+zip")?;
        zip.start_file("META-INF/container.xml", stored)?;
        zip.write_all(br#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#)?;

        let mut manifest = String::from(r#"<item href="style.css" id="css" media-type="text/css"/>"#);
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
            manifest.push_str(&format!(r#"<item href="ch{i}.xhtml" id="ch{i}" media-type="application/xhtml+xml"/>"#));
            let body: String = paragraphs.iter().map(|p| format!("<p>{}</p>", escape(p))).collect();
            zip.start_file(format!("OEBPS/ch{i}.xhtml"), stored)?;
            zip.write_all(format!(r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body>{body}</body></html>"#).as_bytes())?;
        }
        zip.start_file("OEBPS/style.css", stored)?;
        zip.write_all(css.as_bytes())?;
        zip.start_file("OEBPS/content.opf", stored)?;
        zip.write_all(format!(r#"<?xml version="1.0"?><package version="3.0"><manifest>{manifest}</manifest></package>"#).as_bytes())?;
        zip.finish()?;
        Ok(())
    }

    fn read_all(path: &Path) -> Vec<(String, Vec<u8>)> {
        let mut archive = ZipArchive::new(File::open(path).unwrap()).unwrap();
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
            let dir = tempfile::tempdir().unwrap();
            let (input, output) = (dir.path().join("in.epub"), dir.path().join("out.epub"));
            build_epub(&input, &chapters, font.as_deref()).unwrap();

            optimize(&input, &output, &images::Config::default()).unwrap();

            let before = read_all(&input);
            let after = read_all(&output);
            prop_assert_eq!(chapter_texts(&before), chapter_texts(&after));

            // Container rules and the fonts pass, checked on every case.
            prop_assert_eq!(after[0].0.as_str(), "mimetype");
            prop_assert!(after.iter().all(|(name, _)| !fonts::is_font(name)));
            prop_assert!(after.iter().all(|(name, _)| name != "META-INF/encryption.xml"));
            let opf = &after.iter().find(|(n, _)| n.ends_with(".opf")).unwrap().1;
            prop_assert!(!String::from_utf8_lossy(opf).contains("f.ttf"));
        }
    }

    #[test]
    fn refuses_to_overwrite_input() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.epub");
        build_epub(&input, &[vec!["hi".into()]], None).unwrap();
        let before = std::fs::read(&input).unwrap();
        assert!(optimize(&input, &input, &images::Config::default()).is_err());
        assert_eq!(std::fs::read(&input).unwrap(), before);
    }
}
