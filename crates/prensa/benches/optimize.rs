//! Time `prensa::optimize` on real books.
//!
//! The books are not in the repository; `benches/fetch-books.sh` downloads
//! them into `benches/books/`. Every `.epub` in that folder (or in the folder
//! named by `PRENSA_BENCH_BOOKS`) is benchmarked, so any book you own can be
//! dropped in too. See `docs/benchmarks.md` for the books and the baseline.
//!
//! The book is read into memory before timing and written to memory, so the
//! numbers are the engine alone, without disk I/O.

use divan::Bencher;
use divan::counter::BytesCount;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::LazyLock;

/// (file name, contents) of every book to benchmark, sorted by name.
static BOOKS: LazyLock<Vec<(String, Vec<u8>)>> = LazyLock::new(|| {
    let dir = std::env::var_os("PRENSA_BENCH_BOOKS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("benches/books"));
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut books: Vec<(String, Vec<u8>)> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "epub"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let bytes = std::fs::read(&p).unwrap_or_else(|e| panic!("reading {name}: {e}"));
            (name, bytes)
        })
        .collect();
    books.sort();
    books
});

fn main() {
    if BOOKS.is_empty() {
        eprintln!(
            "no books to benchmark: run crates/prensa/benches/fetch-books.sh, \
             or set PRENSA_BENCH_BOOKS to a folder of .epub files"
        );
        return;
    }
    divan::main();
}

fn names() -> impl Iterator<Item = &'static str> {
    BOOKS.iter().map(|(name, _)| name.as_str())
}

/// A whole book, default options. A run takes from tenths of a second to
/// seconds, so a few samples of one run each are enough.
#[divan::bench(args = names(), sample_count = 10, sample_size = 1)]
fn optimize(bencher: Bencher, name: &str) {
    let book = &BOOKS.iter().find(|(n, _)| n == name).unwrap().1;
    let opts = prensa::Options::default();
    bencher.counter(BytesCount::of_slice(book)).bench(|| {
        let mut out = Cursor::new(Vec::with_capacity(book.len()));
        prensa::optimize(Cursor::new(book.as_slice()), &mut out, &opts).unwrap()
    });
}
