function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

self.addEventListener("message", async (event) => {
  if (event.data?.type !== "optimize") {
    return;
  }

  let wasm;
  try {
    wasm = await import("./pkg/prensa_wasm.js");
  } catch (error) {
    self.postMessage({
      type: "error",
      message: `Could not load the WebAssembly package at ./pkg/prensa_wasm.js. Build it with "wasm-pack build crates/prensa-wasm --release --target web --out-dir ../../web/pkg" from the repository root, then serve web over HTTP(S). ${errorMessage(error)}`,
    });
    return;
  }

  try {
    await wasm.default();
  } catch (error) {
    self.postMessage({
      type: "error",
      message: `Could not initialize the WebAssembly optimizer. Rebuild the package and make sure its .wasm file is available. ${errorMessage(error)}`,
    });
    return;
  }

  try {
    const epub = new Uint8Array(event.data.bytes);
    const { bytes, report } = wasm.optimize(epub, event.data.options, (item) => {
      self.postMessage({ type: "progress", ...item });
    });
    const output = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
    self.postMessage({ type: "result", bytes: output, report }, [output]);
  } catch (error) {
    self.postMessage({
      type: "error",
      message: errorMessage(error),
    });
  }
});
