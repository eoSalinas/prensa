# Contributing to prensa

Hello, and thanks for stopping by! prensa makes EPUBs friendlier for small
e-ink readers, and there is room to help whether you write Rust or not. This
guide gets you from "I'd like to help" to a merged pull request.

- [You don't need to write code](#you-dont-need-to-write-code)
- [Finding something to work on](#finding-something-to-work-on)
- [Getting started](#getting-started)
- [How prensa works](#how-prensa-works)
- [Making a change](#making-a-change)
- [Opening a pull request](#opening-a-pull-request)
- [License](#license)

## You don't need to write code

prensa's defaults were tuned by comparing pages side by side on one device: an
Xteink X4 Pro running CrossPoint. **Results from other readers are the most
valuable thing you can contribute.** They decide what device profiles prensa
will offer.

If you have an e-reader, run prensa on a book with images and open an issue
with:

- device and firmware, with versions (e.g. Kobo Clara Colour, KOReader 2026.10)
- the command you ran, including any options
- what got better or worse: sharpness, missing images, page-turn speed,
  file size
- photos of the same page before and after, if you can

A book with diagrams is ideal. [Pro Git](https://github.com/progit/progit2/releases)
and [Alice's Adventures in Wonderland illustrated by Arthur Rackham](https://www.gutenberg.org/ebooks/28885)
are free and are what the README photos use.

Bug reports are just as welcome: an EPUB that fails or comes out wrong, an
unclear error, a confusing option. If the book is not freely shareable, say
where it comes from and what happened; never attach copyrighted books.

## Finding something to work on

- Issues labeled [`good first issue`](https://github.com/eoSalinas/prensa/labels/good%20first%20issue)
  are small and well scoped, a good way to learn the codebase and the flow.
- Issues labeled [`help wanted`](https://github.com/eoSalinas/prensa/labels/help%20wanted)
  are larger and need someone to pick them up.

Before starting on something big, comment on the issue (or open one) so we can
agree on the approach first. It saves you from writing code twice.

## Getting started

You need a Rust toolchain, 1.88 or newer ([rustup](https://rustup.rs/)).

```sh
git clone https://github.com/eoSalinas/prensa
cd prensa
cargo test --workspace
cargo run -p prensa-cli -- book.epub book-optimized.epub
```

The repository has no EPUBs (books are large and usually copyrighted); use any
book you own or one of the free ones above.

## How prensa works

The repository is a Cargo workspace with two crates:

| Crate | Path | What it is |
|---|---|---|
| `prensa` | `crates/prensa` | The engine, a library. Never prints, works on any `Read + Seek` / `Write + Seek` |
| `prensa-cli` | `crates/prensa-cli` | The `prensa` command: paths, atomic writes, progress bar, summary |

An EPUB is a zip archive. `prensa::optimize` reads it entry by entry and
`decide()` in `crates/prensa/src/lib.rs` picks an action for each one:

- **Skip**: drop the entry (embedded fonts; `mimetype` is rewritten first)
- **CopyRaw**: copy the compressed bytes untouched (most of the book)
- **Rewrite**: change a text file (CSS, the OPF manifest, `encryption.xml`)
- **Image**: fit a JPEG or PNG to the screen

The work is split into passes, one module each:

- `fonts.rs`: removes embedded fonts and every reference to them
- `images.rs`: downscales, converts to grayscale, rewrites progressive JPEGs as
  baseline, and keeps the original when rewriting gains nothing

A new optimization usually means a new action in `decide()` or a change inside
one pass.

### Guarantees

Every change must keep these true:

- the input file is never modified
- the visible text of the book and its element structure (tags, in order)
  are identical before and after; only image attributes and image bytes may
  change (`visible_text_and_structure_are_unchanged` in `lib.rs` checks this
  on randomly generated books)
- the output is a valid EPUB container: `mimetype` first and uncompressed

If your change touches how entries are written, run the tests and also try a
real book on a device or in a reader app.

## Making a change

Run what CI runs before pushing:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

A few conventions:

- Code, comments, docs and messages are in English.
- One change per pull request. Found an unrelated bug along the way? It gets its
  own PR.
- Add tests for new behavior. A property test is welcome when the input space
  is large.
- Public library items need doc comments.
- **Breaking the library API?** Bump the version in the same PR (on 0.x, the
  minor). The `semver` check tells you when this applies; see
  [RELEASING.md](RELEASING.md#versioning) for why.

## Opening a pull request

PRs are squash merged, so the **PR title becomes the commit on `main`** and
must follow [Conventional Commits](https://www.conventionalcommits.org/):

```text
feat(cli): add --keep-fonts
fix: keep PNG transparency on white
docs: explain the images pass
```

Common types are `feat`, `fix`, `docs`, `refactor`, `test`, `build`, `ci` and
`chore`, with a lowercase description. Commits inside the branch can be
anything; only the title counts.

The description template has two sections: **Summary** (what changed and
why) and **Testing** (what you ran and what came out). CI checks
formatting, lints, tests, the minimum Rust version, library semver and the
title; all of them must pass before merging.

Don't worry about getting everything right the first time. Review is a
conversation, and small follow-up commits on the same branch are fine.

## License

prensa is dual licensed under [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE). Unless you explicitly state otherwise, any
contribution you intentionally submit for inclusion in prensa, as defined in
the Apache-2.0 license, is dual licensed the same way, without any additional
terms or conditions.
