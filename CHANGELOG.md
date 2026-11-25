# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
uses [Semantic Versioning](https://semver.org/) (below 1.0, a minor version
may break the library API).

## [Unreleased]

## [0.4.0]

prensa is about three times faster: the CLI optimizes images on all cores, and
books with transparent images are quicker even on one. The output is
byte-identical to 0.3.0.

### Added

- A `parallel` feature on `prensa`, off by default, that optimizes images on
  all cores with rayon. `prensa-cli` turns it on: Pro Git goes from 4.1 s to
  1.3 s on 22 threads. Each thread holds a decoded image, so peak memory grows
  with the thread count; `RAYON_NUM_THREADS` limits it. The web page stays
  single-threaded (#69).
- The web page has a favicon (#57).

### Changed

- Images with transparency are flattened onto white with integer math instead
  of per-pixel floats: Pro Git is 18% faster on one thread (#67).

## [0.3.0]

prensa now runs in the browser: a web page optimizes a book on your own
computer through WebAssembly, and each release deploys it to
[GitHub Pages](https://eosalinas.github.io/prensa/). Nothing is uploaded.

### Added

- A web page to optimize an EPUB without installing anything: pick a book and
  a device, set the advanced options if needed, follow the progress and
  download `<name>-optimized.epub` with the report. prensa runs in a Web
  Worker, so the page stays responsive (#49).
- `prensa-wasm`, WebAssembly bindings with a JavaScript API,
  `optimize(epub, options, onProgress)`, returning the optimized bytes and the
  report. Not published to crates.io (#45).
- Each release deploys the web page, built from the release tag, to GitHub
  Pages (#48).

### Changed

- `image` is built with only the codecs prensa uses (JPEG, PNG, GIF, BMP,
  WebP): a faster build and a smaller binary, with byte-identical output
  (#44).

## [0.2.0]

Images the reader could not show now show up, wide diagrams can be turned to
fill the screen, and the library's `Options` is built through a validating
builder (breaking).

### Added

- Convert images CrossPoint cannot show: lossy WebP becomes JPEG; lossless
  WebP and BMP become PNG. GIF becomes PNG when that is smaller or the image
  has to shrink. A converted image is renamed, never over an existing entry
  (`fig.webp` next to `fig.png` becomes `fig-1.png`), and every reference to
  it is updated: `src`, `href`, `xlink:href` and inline `style` in chapters,
  `url()` in CSS, and `href`/`media-type` in the manifest (#34).
- Move images embedded in chapters as `data:` URIs into their own entries,
  listed in the manifest; the same image in several chapters becomes one
  file (#38).
- `--device x4|x3` (`x4-pro` and `x4-classic` are aliases of `x4`) and, in the
  library, `Device` and `OptionsBuilder::device`. The X3 box (484×726) is the
  X4 box scaled to its screen and has not been compared on a device yet
  (#35).
- `--rotate off|cw|ccw`, off by default, and `Rotation`,
  `OptionsBuilder::rotate` and `no_rotate`: an image that would be shrunk to
  less than a third of its size upright, and that turning shows at least 30%
  larger, is turned 90°; its `width`/`height` attributes and inline CSS sizes
  are dropped in chapters (#37).
- `Report::images_failed` (#27), `images_converted` (#34), `images_rotated`
  (#37) and `images_extracted` (#38); the CLI summary shows them.
- The property test also checks that the element structure of every chapter
  is unchanged, so reading positions saved by sync services stay valid (#32).

### Changed

- **Breaking:** `Options` is built with `Options::builder()`, which checks
  every setting at once and returns `InvalidOptions`; its fields are private
  and it is no longer `Copy` (#12, #22, #25).
- **Breaking:** `Report::warnings` is a `Vec<Warning>` with the entry each one
  is about, instead of strings (#23).
- `--max-width` and `--max-height` default to the box of `--device` and
  override it one side at a time (#35).
- CSS, OPF and `encryption.xml` entries with nothing to rewrite are copied as
  stored instead of being written back (#33).
- The CLI buffers writes to the output file (#17).

### Fixed

- Progressive JPEGs with fill bytes before the frame header are detected and
  rewritten as baseline (#13).
- A book whose OPF or `encryption.xml` is not UTF-8 fails with
  `Error::Malformed` instead of keeping removed fonts in them (#14).
- A read error while opening the book is an `Error::Io`, not
  `Error::InvalidZip` (#15).
- The CLI writes the output with the usual file permissions instead of
  `0600` (#16).
- Entries read into memory are capped at 128 MiB: an oversized text entry
  fails the book, an oversized image is copied unchanged with a warning
  (#20).
- Fonts declared only by their manifest media type are removed (#21).
- Image warnings are printed once and on one line (#28).

## [0.1.0]

First release. `prensa` (library) and `prensa-cli` (the `prensa` command)
share one version.

### Added

- Remove embedded fonts: font files, their OPF manifest items, `@font-face`
  rules and font entries in `META-INF/encryption.xml`.
- Fit JPEG and PNG images into 440×733 (never upscaled), in grayscale with
  transparency flattened onto white, sharpened after downscaling; JPEGs are
  written as baseline. An image that does not need to change is kept when
  rewriting it gains nothing.
- CLI options `--max-width`, `--max-height`, `--quality` and `--sharpen`.
- Progress bar and colored summary in a terminal, plain output otherwise;
  respects `NO_COLOR`.
- Library API: `optimize` and `optimize_with_progress` over any
  `Read + Seek` / `Write + Seek`, with `Options`, `Report` and a typed `Error`.
- Guarantees: the input is never modified, the output is written atomically,
  the visible text is unchanged (property-tested) and the output is a valid
  EPUB container.

[Unreleased]: https://github.com/eoSalinas/prensa/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/eoSalinas/prensa/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/eoSalinas/prensa/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/eoSalinas/prensa/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/eoSalinas/prensa/releases/tag/v0.1.0
