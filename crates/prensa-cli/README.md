# prensa-cli

Optimize EPUBs for e-ink readers before copying them to the device. Installs
the `prensa` command.

```sh
cargo install prensa-cli
prensa book.epub book-optimized.epub
```

```text
✓ Fonts    13 removed (0.38 MB)
✓ Images   187 optimized, 1 kept (13.21 MB → 2.28 MB)
✓ Wrote book-optimized.epub  13.99 MB → 2.68 MB (-81%)
```

The output is a smaller, standard EPUB: embedded fonts removed, images fitted
to the screen in grayscale, progressive JPEGs rewritten as baseline. The input
file is never modified.

Defaults are tuned on an Xteink X4 Pro running the CrossPoint firmware. Run
`prensa --help` for options, and see the
[repository](https://github.com/eoSalinas/prensa) for before/after photos and
limitations.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
