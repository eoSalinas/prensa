# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
uses [Semantic Versioning](https://semver.org/) (below 1.0, a minor version
may break the library API).

## [Unreleased]

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

[Unreleased]: https://github.com/eoSalinas/prensa/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/eoSalinas/prensa/releases/tag/v0.1.0
