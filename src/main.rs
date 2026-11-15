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

    let mut stats = Stats::default();
    for i in 0..archive.len() {
        // `by_index_raw` borrows `archive` mutably: one entry at a time.
        // `into_owned` copies the name so the borrow ends here.
        let (name, size) = {
            let entry = archive.by_index_raw(i)?;
            (entry.name()?.into_owned(), entry.size())
        };

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
