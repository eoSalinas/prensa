# prensa-wasm

WebAssembly bindings for [prensa](../prensa), the EPUB optimizer for e-ink
readers. They let the web page optimize a book in the browser: nothing is
uploaded. Not published to crates.io.

## Build

Needs [`wasm-pack`](https://github.com/drager/wasm-pack):

```sh
wasm-pack build crates/prensa-wasm --release --target web
```

This writes `crates/prensa-wasm/pkg/` (ignored by git): `prensa_wasm.js`, the
`.wasm` file and TypeScript types (`prensa_wasm.d.ts`). Pass
`--out-dir <dir>` to put it somewhere else.

## Use

```js
import init, { optimize } from "./pkg/prensa_wasm.js";
await init();

const { bytes, report } = optimize(epubBytes, { device: "x3" }, (p) => {
  console.log(`${p.done}/${p.total} ${p.entry}`);
});
```

- `epubBytes`: a `Uint8Array` with the input EPUB.
- `options` (optional; every field optional, omitted means the default):
  - `device`: `"x4"` (default) or `"x3"`
  - `maxWidth`, `maxHeight`: override the device's box; a side not given
    keeps the device's
  - `jpegQuality`: 1-100
  - `sharpen`: unsharp-mask sigma, or `null` to turn sharpening off
  - `rotate`: `"clockwise"`, `"counterclockwise"`, or `null` (the default)
    to never turn images
- `onProgress({ done, total, entry })` (optional): called before each entry,
  `done` from `0` to `total - 1`. The book is finished when `optimize` returns.
- Returns `{ bytes, report }`: the optimized EPUB as a `Uint8Array`, and
  `report` with `fontsRemoved`, `fontBytes`, `imagesChanged`,
  `imagesConverted`, `imagesRotated`, `imagesExtracted`, `imagesKept`,
  `imagesFailed`, `imageBytesBefore`, `imageBytesAfter` and
  `warnings: [{ entry, message }]`.
- Throws an `Error` for invalid options (including unknown keys, to catch
  typos) or input that is not an EPUB. If `onProgress` throws, its first
  exception is rethrown as is once the book is done.

`optimize` is synchronous and CPU-bound: call it from a Web Worker so the page
stays responsive. It runs on one thread, and the input and output are both held
in memory.

If prensa panics (a bug), the module instance is unusable afterwards; create a
new one (a new worker) before the next book.

Output can differ from the native CLI's by rounding in a few pixels (at most a
few gray levels): floating-point math in resizing and sharpening is not
bit-identical between x86 and WebAssembly.

## Test

```sh
wasm-pack test --node crates/prensa-wasm
```
