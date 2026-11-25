# Benchmarks

`cargo bench -p prensa` times `prensa::optimize` on whole books, with the
default options. Use it to check whether a performance change helps, and
measure before optimizing: the baseline below says where the time goes.

## Running

The books are free but not in the repository. Fetch them once (needs curl):

```sh
crates/prensa/benches/fetch-books.sh
cargo bench -p prensa
```

They are real books, used as published:

| Book | What it exercises |
|---|---|
| `progit.epub` | [Pro Git](https://github.com/progit/progit2) 2.1.450 (CC BY-NC-SA 3.0), 14 MB: 188 PNG diagrams and screenshots and 13 embedded fonts. |
| `oz.epub` | [The Wonderful Wizard of Oz](https://www.gutenberg.org/ebooks/43936) (public domain), 10.7 MB: 148 color JPEG illustrations. |
| `moby-dick.epub` | [Moby Dick](https://www.gutenberg.org/ebooks/2701) (public domain), 0.7 MB: a long text with one image, so the cost of everything but images. |

WebP and GIF are rare in free EPUBs and we found none to use, so converting
them is not measured here.

`cargo bench -p prensa` builds the library as it is by default, one image at
a time. Add `--features parallel` to measure it the way the CLI runs:

```sh
cargo bench -p prensa --features parallel
```

Any other `.epub` in `crates/prensa/benches/books/` is benchmarked too; set
`PRENSA_BENCH_BOOKS` to use another folder. Books are read into memory before
timing and written to memory, so disk speed does not count.

On a laptop with performance and efficiency cores, runs that land on different
cores differ by 50% or more. Pin the run to one performance core to compare
two versions (without `parallel`, which needs every core):

```sh
taskset -c 1 cargo bench -p prensa
```

## Baseline

prensa 0.3.0 (`a05d565`), Intel Core Ultra 7 155H, Linux, pinned to one
performance core (`taskset -c 1`), Rust 1.100. Median of 10 runs:

| Book | Size | Time | Throughput |
|---|---|---|---|
| `progit.epub` | 13.99 MB | 3.5 s | 4.0 MB/s |
| `oz.epub` | 10.71 MB | 1.6 s | 6.6 MB/s |
| `moby-dick.epub` | 0.70 MB | 55–80 ms | 9–13 MB/s |

With `--features parallel` (all 22 threads, not pinned), on the commit that
added it:

| Book | One thread | Parallel | Speedup |
|---|---|---|---|
| `progit.epub` | 4.06 s | 1.30 s | 3.1× |
| `oz.epub` | 1.97 s | 0.64 s | 3.1× |
| `moby-dick.epub` | 71 ms | 70 ms | 1× (one image) |

The speedup is well below the thread count: this laptop has 6 performance
cores (12 threads) and slower efficiency cores, and clocks drop when every
core is busy.

Peak memory grows with the number of threads, since each one holds a decoded
image: optimizing Pro Git with the CLI peaks at 66 MB on one thread, 228 MB
on 8 and 524 MB on 22, while 8 threads are only 0.2 s slower than 22. Set
`RAYON_NUM_THREADS` to use fewer threads.

Moby Dick is short enough that its median moves between runs; compare it over
a few runs.

SHA-256 of the books measured (Gutenberg rebuilds its files now and then, so
yours may differ slightly):

```
ed3e5e9f5b7da9c05fae7ac2fcfaaa887e5df196a4a07b59ffed832818f73ae1  progit.epub
353c48670f2caa45b5dad1cab007514f7eef921a109b057c855c8cba5e23ad6d  oz.epub
3ed3abe693b2a59acfbf49e01043f79b508a3c344cf2667b9586bb17e9af943c  moby-dick.epub
```

## Where the time goes

Profiled with [samply](https://github.com/mstange/samply) on the same build,
grouping each sample by the pipeline stage on its stack. Share of the time per
book:

| Stage | `progit` | `oz` | `moby-dick` |
|---|---|---|---|
| Resize (Lanczos3) | 43% | 53% | 49% |
| Grayscale (incl. flattening alpha onto white) | 28% | 10% | 19% |
| Encode (PNG for `progit`, JPEG for the others) | 15% | 17% | 4% |
| Decode | 10% | 11% | 12% |
| Sharpen | 3% | 8% | 2% |
| Zip read and write | 1% | <1% | 6% |
| Text rewrites (OPF, CSS, chapters) | <1% | <1% | <1% |

Top hotspots:

1. **Images are nearly all of it.** Even in Moby Dick, its one image (the
   cover) takes most of the run. Each image is processed on its own, so
   processing several at once should scale with the number of cores.
2. **Resizing** in `image::imageops::resize` is the largest single stage.
3. **`roundf`**: `f32::round` compiles to a call into libm, because the
   default x86-64 target has no SSE4.1 rounding instruction. It is 11% of
   Pro Git's time and 22% of Oz's, called from resize, grayscale conversion,
   sharpening and the JPEG encoder.
4. **Flattening alpha** in `to_screen_gray` (`crates/prensa/src/images.rs`)
   goes pixel by pixel through `get_pixel` and `round`; Pro Git's PNGs have
   alpha, so it is 28% of that book.
5. **PNG encoding** at `CompressionType::Best`: zlib-rs's
   `longest_match_slow` alone is 13% of Pro Git.

To profile yourself (Linux; the two `sysctl` settings last until reboot):

```sh
sudo sysctl kernel.perf_event_paranoid=1 kernel.perf_event_mlock_kb=2048
CARGO_PROFILE_BENCH_DEBUG=true cargo bench -p prensa --no-run
# then record the binary `--no-run` prints for benches/optimize.rs:
samply record target/release/build/prensa/<hash>/out/optimize-<hash> --bench progit
```

`--bench progit` runs only the books whose name matches.
