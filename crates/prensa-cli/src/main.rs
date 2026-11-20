//! `prensa` CLI: file paths, atomic writes and output. The work happens in the library.
//!
//! Progress and warnings go to stderr, the summary to stdout. Colors and the
//! progress bar turn off when the stream is not a terminal or `NO_COLOR` is set.

use anyhow::{Context, Result, bail};
use clap::Parser;
use console::{Term, style};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use prensa::{Options, Progress, Report};
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
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
    #[arg(long, default_value_t = Options::default().max_width(), value_parser = clap::value_parser!(u32).range(1..))]
    max_width: u32,
    /// Maximum image height, in pixels
    #[arg(long, default_value_t = Options::default().max_height(), value_parser = clap::value_parser!(u32).range(1..))]
    max_height: u32,
    /// JPEG quality (1-100)
    #[arg(long, default_value_t = Options::default().jpeg_quality(), value_parser = clap::value_parser!(u8).range(1..=100))]
    quality: u8,
    /// Sharpening after downscaling (unsharp mask sigma; 0 disables)
    #[arg(long, default_value_t = Options::default().sharpen().unwrap_or(0.0))]
    sharpen: f32,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // https://no-color.org: any non-empty value disables colors.
    if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        console::set_colors_enabled(false);
        console::set_colors_enabled_stderr(false);
    }

    let builder = Options::builder()
        .max_size(cli.max_width, cli.max_height)
        .jpeg_quality(cli.quality);
    // `--sharpen 0` is how the command line turns sharpening off.
    let builder = if cli.sharpen == 0.0 {
        builder.no_sharpen()
    } else {
        builder.sharpen(cli.sharpen)
    };
    let opts = builder.build()?;

    let stderr = Term::stderr();
    if stderr.is_term() {
        eprintln!("Optimizing {}", style(cli.input.display()).bold());
    }
    let bar = progress_bar(&stderr);
    let result = optimize_file(&cli.input, &cli.output, &opts, |p| {
        bar.set_length(p.total as u64);
        bar.set_position(p.done as u64);
        bar.set_message(p.entry.to_owned());
    });
    // Clear the bar before printing anything else, including errors.
    bar.finish_and_clear();

    let sizes = result?;
    print_summary(&cli.output, &sizes);
    Ok(())
}

/// A bar on stderr, hidden when stderr is not a terminal (pipes, files, CI).
fn progress_bar(stderr: &Term) -> ProgressBar {
    if !stderr.is_term() {
        return ProgressBar::hidden();
    }
    let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
    bar.set_style(
        ProgressStyle::with_template("  {bar:30.cyan/dim} {pos:>3}/{len}  {wide_msg:.dim}")
            .expect("valid template")
            .progress_chars("━━─"),
    );
    bar
}

/// What the summary needs: the library report plus file sizes on disk.
struct Sizes {
    report: Report,
    input_bytes: u64,
    output_bytes: u64,
}

/// Optimize `input` into `output`. The input is never modified, and `output`
/// is either fully written or untouched.
fn optimize_file(
    input: &Path,
    output: &Path,
    opts: &Options,
    on_progress: impl FnMut(Progress<'_>),
) -> Result<Sizes> {
    let input_real = input
        .canonicalize()
        .with_context(|| format!("opening {}", input.display()))?;
    if input_real == output.canonicalize().unwrap_or_default() {
        bail!("output must not be the input file");
    }
    let file = File::open(input).with_context(|| format!("opening {}", input.display()))?;
    let input_meta = file.metadata()?;
    let input_bytes = input_meta.len();

    // Atomic write: write to a temp file in the same directory and rename at the end.
    // If anything fails midway, the temp file is deleted and `output` is never half-written.
    let dir = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    // `ZipWriter` issues many small writes (local headers, the central
    // directory); buffer them instead of making a syscall for each.
    let mut out = BufWriter::new(tmp.as_file_mut());
    let report = prensa::optimize_with_progress(BufReader::new(file), &mut out, opts, on_progress)?;
    out.flush()?;
    drop(out);
    let output_bytes = tmp.as_file().metadata()?.len();
    // The temp file is created private (0600 on Unix) and `persist` keeps that
    // mode. Give the output the permissions of the file it replaces, or of the
    // input for a new file, so other users and services can still read it.
    let permissions = match std::fs::metadata(output) {
        Ok(existing) => existing.permissions(),
        Err(_) => input_meta.permissions(),
    };
    tmp.as_file().set_permissions(permissions)?;
    tmp.persist(output)
        .with_context(|| format!("writing {}", output.display()))?;
    Ok(Sizes {
        report,
        input_bytes,
        output_bytes,
    })
}

fn mb(bytes: u64) -> String {
    format!("{:.2} MB", bytes as f64 / 1e6)
}

fn print_summary(output: &Path, sizes: &Sizes) {
    let r = &sizes.report;
    let ok = style("✓").green().bold();

    for warning in &r.warnings {
        eprintln!("{} {warning}", style("!").yellow().bold());
    }

    let fonts = if r.fonts_removed == 0 {
        style("none embedded".to_string()).dim().to_string()
    } else {
        format!(
            "{} removed ({})",
            style(r.fonts_removed).bold(),
            mb(r.font_bytes)
        )
    };
    println!("{ok} {:<8} {fonts}", "Fonts");

    let total_images = r.images_changed + r.images_kept + r.images_failed;
    let images = if total_images == 0 {
        style("none".to_string()).dim().to_string()
    } else {
        let converted = match r.images_converted {
            0 => String::new(),
            n => format!(" ({n} converted to JPEG/PNG)"),
        };
        format!(
            "{} optimized{converted}, {} kept ({} → {})",
            style(r.images_changed).bold(),
            r.images_kept,
            mb(r.image_bytes_before),
            mb(r.image_bytes_after)
        )
    };
    println!("{ok} {:<8} {images}", "Images");

    let change = if sizes.input_bytes == 0 {
        0.0
    } else {
        (sizes.output_bytes as f64 / sizes.input_bytes as f64 - 1.0) * 100.0
    };
    let change = format!("{change:+.0}%");
    let change = if sizes.output_bytes < sizes.input_bytes {
        style(change).green().bold()
    } else {
        style(change).yellow().bold()
    };
    println!(
        "{ok} Wrote {}  {} → {} ({change})",
        style(output.display()).bold(),
        mb(sizes.input_bytes),
        mb(sizes.output_bytes),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// A minimal valid EPUB on disk.
    fn write_epub(path: &Path) {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("mimetype", stored).unwrap();
        zip.write_all(b"application/epub+zip").unwrap();
        zip.finish().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn new_output_gets_the_input_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let (input, output) = (dir.path().join("in.epub"), dir.path().join("out.epub"));
        write_epub(&input);
        set_mode(&input, 0o644);
        optimize_file(&input, &output, &Options::default(), |_| {}).unwrap();
        assert_eq!(mode(&output), 0o644);
    }

    #[cfg(unix)]
    #[test]
    fn replaced_output_keeps_its_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let (input, output) = (dir.path().join("in.epub"), dir.path().join("out.epub"));
        write_epub(&input);
        set_mode(&input, 0o600);
        std::fs::write(&output, b"old").unwrap();
        set_mode(&output, 0o664);
        optimize_file(&input, &output, &Options::default(), |_| {}).unwrap();
        assert_eq!(mode(&output), 0o664);
    }

    #[test]
    fn refuses_to_overwrite_input() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.epub");
        std::fs::write(&input, b"original").unwrap();
        assert!(optimize_file(&input, &input, &Options::default(), |_| {}).is_err());
        assert_eq!(std::fs::read(&input).unwrap(), b"original");
    }
}
