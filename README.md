# prensa

Optimize EPUBs for e-ink readers before copying them to the device.

Most EPUBs are made for phones and computers: large color images, progressive
JPEGs, embedded fonts. Small e-ink readers with little memory struggle with
them. `prensa` writes an optimized copy of the book that is smaller, sharper
and easier on the device, and it never touches the original file.

It is tuned on an **Xteink X4 Pro** running the
[CrossPoint](https://github.com/crosspoint-reader/crosspoint-reader) firmware,
but the output is still a standard EPUB that opens in any reader.

## Before and after

Photos of the same page on an Xteink X4 Pro (CrossPoint), original book on the
left, `prensa` output on the right.

### Missing image → visible

Wide PNG diagrams overflow CrossPoint's PNG line buffer and are silently
dropped. `prensa` downscales them, so they show up.

| Before | After |
|---|---|
| ![Pro Git, Figure 10, before: empty frame](docs/images/progit-fig10-before.jpg) | ![Pro Git, Figure 10, after: diagram visible](docs/images/progit-fig10-after.jpg) |

*Pro Git, Figure 10. 24 of the book's 188 images are dropped by the firmware; after `prensa`, all of them render.*

### Tiny diagram → readable (optional)

Figure 10 now shows, but at 440 px wide its small text is hard to read. With
`--rotate cw`, diagrams that would be shrunk to less than a third of their
size are turned to fill the screen's height instead, up to 1.67× larger. Turn
the device to read them. Off by default.

| `prensa` | `prensa --rotate cw` |
|---|---|
| ![Pro Git, Figure 10, upright: small text](docs/images/progit-fig10-after.jpg) | ![Pro Git, Figure 10, turned: fills the screen's height](docs/images/progit-fig10-rotated.jpg) |

*Pro Git, Figure 10. 68 of the book's 188 images are turned; the rest stay upright.*

### Unreadable → readable

Large diagrams are downscaled on the device with a simple fixed-point scaler,
which mangles small text. `prensa` does the same downscale on your computer
with a high-quality filter and light sharpening.

| Before | After |
|---|---|
| ![Mastering Bitcoin, Figure 2-2, before: jagged text](docs/images/mastering-bitcoin-fig2-2-before.jpg) | ![Mastering Bitcoin, Figure 2-2, after: clean text](docs/images/mastering-bitcoin-fig2-2-after.jpg) |

*Mastering Bitcoin, Figure 2-2.*

### Blurry → sharp

CrossPoint decodes progressive JPEGs at 1/8 resolution. `prensa` re-encodes
them as baseline JPEGs.

| Before | After |
|---|---|
| ![Alice illustration, before: blocky and blurry](docs/images/alice-progressive-before.jpg) | ![Alice illustration, after: sharp](docs/images/alice-progressive-after.jpg) |

*Alice's Adventures in Wonderland, illustrated by Arthur Rackham (images converted to progressive JPEG to reproduce the issue).*

### File size

| Book | Original | Optimized | Reduction |
|---|---|---|---|
| Pro Git (2.1.450) | 13.99 MB | 2.68 MB | −81% |
| Mastering Bitcoin (3rd ed.) | 6.6 MB | 2.0 MB | −70% |

Each book takes about a second or less to process, a few seconds for Pro Git.

## Install

Requires a Rust toolchain (1.88 or newer).

```sh
cargo install prensa-cli
```

This installs the `prensa` command. To build the latest unreleased version
from the repository instead:

```sh
cargo install --git https://github.com/eoSalinas/prensa prensa-cli
```

## Usage

```sh
prensa book.epub book-optimized.epub
```

```text
✓ Fonts    13 removed (0.38 MB)
✓ Images   187 optimized, 1 kept (13.21 MB → 2.28 MB)
✓ Wrote book-optimized.epub  13.99 MB → 2.68 MB (-81%)
```

In a terminal it also shows a progress bar and colors; both turn off when the
output is piped or `NO_COLOR` is set.

Then copy `book-optimized.epub` to the device.

### Options

| Option | Default | Description |
|---|---|---|
| `--device <x4\|x3>` | `x4` | E-reader to fit images to: Xteink X4, X4 Pro or X4 Classic (440×733; `x4-pro` and `x4-classic` also work), or X3 (484×726) |
| `--max-width <PX>` | from `--device` | Maximum image width |
| `--max-height <PX>` | from `--device` | Maximum image height |
| `--quality <1-100>` | `92` | JPEG quality |
| `--sharpen <SIGMA>` | `0.8` | Unsharp mask after downscaling; `0` disables it |
| `--rotate <off\|cw\|ccw>` | `off` | Turn wide diagrams 90° to fill the screen's height (see below) |

The defaults won a side-by-side comparison on the device. They favor
legibility over the smallest possible file.

## Library

The optimizer is a library crate, `prensa`; the CLI is a thin front end over
it. `optimize` works on any `Read + Seek` input and `Write + Seek` output
(files, in-memory buffers), never prints, and returns a report.

```toml
[dependencies]
prensa = "0.2"
```

```rust
use std::fs::File;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = prensa::Options::builder()
        .max_size(600, 800) // e.g. a larger screen
        .build()?;

    let input = File::open("book.epub")?;
    let output = File::create("book-optimized.epub")?;
    let report = prensa::optimize(input, output, &opts)?;

    println!("{} image(s) optimized", report.images_changed);
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(())
}
```

Errors are a `prensa::Error`: `InvalidZip`, `Malformed { entry, .. }` (an
entry that must be rewritten could not be parsed) or `Io`. Unlike the CLI,
`optimize` writes straight to `output`; write to a temporary file and rename
it if you need atomic writes.

| Crate | Path | |
|---|---|---|
| `prensa` | `crates/prensa` | the engine |
| `prensa-cli` | `crates/prensa-cli` | the `prensa` binary |
| `prensa-wasm` | `crates/prensa-wasm` | WebAssembly bindings for the web page (not on crates.io) |

## What it does

- **Fonts:** removes embedded font files, their manifest entries, `@font-face`
  rules and font entries in `META-INF/encryption.xml`. CrossPoint ignores
  `@font-face` and always renders with its own fonts, so they are dead weight.
- **Images:** fits every JPEG, PNG, GIF, WebP and BMP into 440×733 (never
  upscales), converts it to grayscale (flattening transparency onto white),
  and writes JPEGs as baseline. If the result is not smaller and the image did
  not need to change, the original is kept.
- **Embedded images:** images written inside a chapter as `data:` URIs
  (`<img src="data:image/png;base64,...">`) never show on CrossPoint. Each one
  moves to a file next to the chapter, goes through the same image steps, and
  is listed in the manifest.
- **Wide diagrams** (`--rotate cw` or `ccw`, off by default): an image that
  would be shrunk to less than a third of its size to fit the width is turned
  90° when that shows it at least 30% larger (up to 1.67× on 440×733), and its
  `width`/`height` in the chapter are dropped. You turn the device to read it.
  On Pro Git, 68 of 188 images are turned; on Mastering Bitcoin, none.
- **Formats:** CrossPoint shows only JPEG, PNG and GIF, and picks the decoder
  from the file extension. WebP and BMP images are converted (lossy WebP to
  JPEG; lossless WebP and BMP to PNG) and renamed, and every reference to them
  in chapters, CSS and the manifest is updated. GIF becomes PNG when that is
  smaller or the image has to shrink.

## Guarantees

- The input file is never modified; the output is written atomically.
- The visible text of the book and the elements around it (paragraphs,
  headings, images, in order) are identical before and after, so reading
  positions saved by sync services such as KOReader's stay valid. This is
  checked by a property-based test over randomly generated books.
- The output is a valid EPUB container (`mimetype` first and uncompressed).

## Limitations

- Tuned for CrossPoint on a 480×800 screen (Xteink X4 Pro; the X4 and X4
  Classic share its panel). The X3 profile
  scales the X4 box to its 528×792 screen and has not been compared on the
  device yet. Readers that use embedded fonts
  (e.g. KOReader) will fall back to their own fonts after `prensa` removes them.
- Very wide diagrams become visible but their smallest text can still be hard
  to read at 440 px. `--rotate cw` helps up to 1.67×; panoramic diagrams stay
  small.
- Thin light-gray lines can look dashed after the device's 4-level dithering.
- Images are converted to grayscale, so color e-ink screens (e.g. Kobo Libra Colour) lose color

## Credits

Photos show pages from:

- [*Pro Git*](https://git-scm.com/book) by Scott Chacon and Ben Straub, licensed
  under [CC BY-NC-SA 3.0](https://creativecommons.org/licenses/by-nc-sa/3.0/).
- [*Mastering Bitcoin*](https://github.com/bitcoinbook/bitcoinbook) by Andreas M.
  Antonopoulos and David A. Harding, licensed under
  [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/).
- *Alice's Adventures in Wonderland* by Lewis Carroll, illustrated by Arthur
  Rackham ([Project Gutenberg #28885](https://www.gutenberg.org/ebooks/28885)),
  public domain.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.

The photos in `docs/images/` show pages from the books listed in
[Credits](#credits) and remain under those books' licenses.
