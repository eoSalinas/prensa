//! `prensa` CLI: file paths, atomic writes and output. The work happens in the library.

use anyhow::{Context, Result, bail};
use clap::Parser;
use prensa::{Options, Report};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

/// Optimize EPUBs for e-ink readers. Writes a copy; the input is never modified.
#[derive(Parser)]
#[command(name = "prensa", version)]
struct Cli {
    /// Input EPUB
    input: PathBuf,
    /// Output EPUB
    output: PathBuf,
    /// Maximum image width, in pixels
    #[arg(long, default_value_t = Options::default().max_width)]
    max_width: u32,
    /// Maximum image height, in pixels
    #[arg(long, default_value_t = Options::default().max_height)]
    max_height: u32,
    /// JPEG quality (1-100)
    #[arg(long, default_value_t = Options::default().jpeg_quality, value_parser = clap::value_parser!(u8).range(1..=100))]
    quality: u8,
    /// Sharpening after downscaling (unsharp mask sigma; 0 disables)
    #[arg(long, default_value_t = Options::default().sharpen)]
    sharpen: f32,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // `Options` is `#[non_exhaustive]`: built from the default, then changed field by field.
    let mut opts = Options::default();
    opts.max_width = cli.max_width;
    opts.max_height = cli.max_height;
    opts.jpeg_quality = cli.quality;
    opts.sharpen = cli.sharpen;

    let report = optimize_file(&cli.input, &cli.output, &opts)?;
    print_report(&report);
    Ok(())
}

/// Optimize `input` into `output`. The input is never modified, and `output`
/// is either fully written or untouched.
fn optimize_file(input: &Path, output: &Path, opts: &Options) -> Result<Report> {
    let input_real = input
        .canonicalize()
        .with_context(|| format!("opening {}", input.display()))?;
    if input_real == output.canonicalize().unwrap_or_default() {
        bail!("output must not be the input file");
    }
    let reader =
        BufReader::new(File::open(input).with_context(|| format!("opening {}", input.display()))?);

    // Atomic write: write to a temp file in the same directory and rename at the end.
    // If anything fails midway, the temp file is deleted and `output` is never half-written.
    let dir = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    let report = prensa::optimize(reader, &mut tmp, opts)?;
    tmp.persist(output)
        .with_context(|| format!("writing {}", output.display()))?;
    Ok(report)
}

fn print_report(report: &Report) {
    let mb = |b: u64| b as f64 / 1e6;
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    println!(
        "fonts removed: {} file(s), {:.1} MB",
        report.fonts_removed,
        mb(report.font_bytes)
    );
    println!(
        "images: {} optimized, {} kept, {:.1} -> {:.1} MB",
        report.images_changed,
        report.images_kept,
        mb(report.image_bytes_before),
        mb(report.image_bytes_after)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_overwrite_input() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.epub");
        std::fs::write(&input, b"original").unwrap();
        assert!(optimize_file(&input, &input, &Options::default()).is_err());
        assert_eq!(std::fs::read(&input).unwrap(), b"original");
    }
}
