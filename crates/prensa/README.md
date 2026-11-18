# prensa

The engine behind the [`prensa`](https://crates.io/crates/prensa-cli) CLI:
optimize EPUBs for e-ink readers. It removes embedded fonts, fits images to
the screen in grayscale and rewrites progressive JPEGs as baseline, producing
a smaller, standard EPUB that is easier on small devices.

Defaults are tuned on an Xteink X4 Pro running the CrossPoint firmware. See
the [repository](https://github.com/eoSalinas/prensa) for before/after photos
and details.

```rust
use std::fs::File;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = File::open("book.epub")?;
    let output = File::create("book-optimized.epub")?;
    let report = prensa::optimize(input, output, &prensa::Options::default())?;

    println!("{} image(s) optimized", report.images_changed);
    Ok(())
}
```

`optimize` works on any `Read + Seek` input and `Write + Seek` output, never
prints and returns a `Report`. Use `optimize_with_progress` to drive a
progress bar.
