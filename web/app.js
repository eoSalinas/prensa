const fileInput = document.querySelector("#file");
const dropZone = document.querySelector("#drop");
const fileName = document.querySelector("#file-name");
const optionsForm = document.querySelector("#options");
const runButton = document.querySelector("#run");
const maxSizeInput = document.querySelector("#maxSize");
const qualityInput = document.querySelector("#quality");
const sharpenInput = document.querySelector("#sharpen");
const rotateInput = document.querySelector("#rotate");
const progressBox = document.querySelector("#progress-box");
const progress = document.querySelector("#progress");
const percent = document.querySelector("#percent");
const errorBox = document.querySelector("#error");
const resultBox = document.querySelector("#result");
const sizeBefore = document.querySelector("#size-before");
const sizeAfter = document.querySelector("#size-after");
const resultSummary = document.querySelector("#result-summary");
const downloadLink = document.querySelector("#download");
const reportList = document.querySelector("#report");
const warningsBox = document.querySelector("#warnings");
const warningList = document.querySelector("#warning-list");
const liveRegion = document.querySelector("#live");

let selectedFile = null;
let worker = null;
let downloadUrl = null;
let busy = false;

function clearResult() {
  resultBox.hidden = true;
  sizeBefore.textContent = "";
  sizeAfter.textContent = "";
  resultSummary.textContent = "";
  reportList.replaceChildren();
  warningList.replaceChildren();
  warningsBox.hidden = true;
  downloadLink.removeAttribute("href");
  if (downloadUrl !== null) {
    URL.revokeObjectURL(downloadUrl);
    downloadUrl = null;
  }
}

function showError(message) {
  errorBox.textContent = message;
  errorBox.hidden = false;
  liveRegion.textContent = message;
}

function clearError() {
  errorBox.textContent = "";
  errorBox.hidden = true;
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

function resetProgress() {
  progress.value = 0;
  percent.textContent = "0%";
}

function setBusy(value) {
  busy = value;
  fileInput.disabled = value;
  runButton.disabled = value || selectedFile === null;
  runButton.textContent = value ? "Optimizing…" : "Optimize book";
  if (value) {
    resetProgress();
    progressBox.hidden = false;
  } else {
    progressBox.hidden = true;
  }
}

function selectFile(file) {
  clearError();
  clearResult();
  resetProgress();
  progressBox.hidden = true;
  liveRegion.textContent = "";

  if (!file || !file.name.toLowerCase().endsWith(".epub")) {
    selectedFile = null;
    fileName.hidden = true;
    fileName.textContent = "";
    dropZone.classList.remove("has-file");
    runButton.disabled = true;
    if (file) {
      showError("Choose a file with the .epub extension.");
    }
    return;
  }

  selectedFile = file;
  fileName.textContent = `${file.name} · ${formatBytes(file.size)}`;
  fileName.hidden = false;
  dropZone.classList.add("has-file");
  runButton.disabled = busy;
  liveRegion.textContent = `${file.name} selected.`;
}

function makeOptions() {
  const options = {
    device: optionsForm.elements.device.value,
    jpegQuality: Number(qualityInput.value),
    sharpen: Number(sharpenInput.value) === 0
      ? null
      : Number(sharpenInput.value) / 100,
    rotate: rotateInput.value === "auto" ? "clockwise" : null,
  };
  const maxSize = maxSizeInput.valueAsNumber;
  if (Number.isFinite(maxSize)) {
    options.maxWidth = maxSize;
    options.maxHeight = maxSize;
  }
  return options;
}

function formatCount(value) {
  return new Intl.NumberFormat().format(value);
}

function formatBytes(value) {
  if (value < 1000) {
    return `${formatCount(value)} bytes`;
  }
  const units = ["kB", "MB", "GB"];
  let amount = value / 1000;
  let unitIndex = 0;
  while (amount >= 1000 && unitIndex < units.length - 1) {
    amount /= 1000;
    unitIndex += 1;
  }
  return `${new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(amount)} ${units[unitIndex]}`;
}

function formatSizeChange(originalSize, optimizedSize) {
  const difference = originalSize - optimizedSize;
  if (difference === 0) {
    return "same size";
  }
  if (originalSize === 0) {
    return null;
  }
  const percentage = new Intl.NumberFormat(undefined, {
    style: "percent",
    maximumFractionDigits: 0,
  }).format(Math.abs(difference) / originalSize);
  return difference > 0 ? `${percentage} smaller` : `${percentage} larger`;
}

function plural(count, one, many) {
  return `${formatCount(count)} ${count === 1 ? one : many}`;
}

function addReportRow(label, value) {
  const term = document.createElement("dt");
  term.textContent = label;
  const description = document.createElement("dd");
  description.textContent = value;
  reportList.append(term, description);
}

function showResult(file, bytes, report) {
  const output = new Blob([bytes], { type: "application/epub+zip" });
  downloadUrl = URL.createObjectURL(output);
  const baseName = file.name.replace(/\.epub$/i, "") || "book";
  downloadLink.href = downloadUrl;
  downloadLink.download = `${baseName}-optimized.epub`;

  sizeBefore.textContent = formatBytes(file.size);
  sizeAfter.textContent = formatBytes(bytes.byteLength);

  // One line people can read (and share) at a glance.
  const summary = [];
  const change = formatSizeChange(file.size, bytes.byteLength);
  if (change !== null) {
    summary.push(change);
  }
  if (report.imagesChanged > 0) {
    summary.push(`${plural(report.imagesChanged, "image", "images")} optimized`);
  }
  if (report.fontsRemoved > 0) {
    summary.push(`${plural(report.fontsRemoved, "font", "fonts")} removed`);
  }
  resultSummary.textContent = summary.length > 0
    ? summary.join(" · ")
    : "Nothing needed to change.";

  // The details list only what happened.
  const rows = [
    ["Images optimized", report.imagesChanged],
    ["Converted to JPEG or PNG", report.imagesConverted],
    ["Turned to fill the screen", report.imagesRotated],
    ["Moved out of chapters", report.imagesExtracted],
    ["Kept as they were", report.imagesKept],
    ["Could not be processed", report.imagesFailed],
    ["Fonts removed", report.fontsRemoved],
  ];
  if (report.imageBytesBefore > 0) {
    addReportRow("Images", `${formatBytes(report.imageBytesBefore)} → ${formatBytes(report.imageBytesAfter)}`);
  }
  for (const [label, count] of rows) {
    if (count > 0) {
      addReportRow(label, formatCount(count));
    }
  }

  if (report.warnings.length > 0) {
    for (const warning of report.warnings) {
      const item = document.createElement("li");
      item.textContent = `${warning.entry}: ${warning.message}`;
      warningList.append(item);
    }
    warningsBox.hidden = false;
  }

  resultBox.hidden = false;
  resultBox.focus();
  liveRegion.textContent = `Your book is ready: ${sizeBefore.textContent} to ${sizeAfter.textContent}.`;
}

function stopWorker() {
  if (worker !== null) {
    worker.terminate();
    worker = null;
  }
}

function finishWithError(message) {
  stopWorker();
  setBusy(false);
  showError(message);
}

fileInput.addEventListener("change", () => {
  selectFile(fileInput.files?.[0] ?? null);
});

dropZone.addEventListener("dragover", (event) => {
  event.preventDefault();
  if (!busy) {
    dropZone.classList.add("over");
  }
});

dropZone.addEventListener("dragleave", (event) => {
  if (!dropZone.contains(event.relatedTarget)) {
    dropZone.classList.remove("over");
  }
});

dropZone.addEventListener("drop", (event) => {
  event.preventDefault();
  dropZone.classList.remove("over");
  if (!busy) {
    selectFile(event.dataTransfer?.files?.[0] ?? null);
  }
});

optionsForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  if (busy || selectedFile === null || !optionsForm.reportValidity()) {
    return;
  }

  const file = selectedFile;
  clearError();
  clearResult();
  setBusy(true);
  liveRegion.textContent = "Preparing the EPUB optimizer.";

  try {
    const bytes = await file.arrayBuffer();
    worker = new Worker(new URL("./worker.js", import.meta.url), { type: "module" });
    worker.addEventListener("message", (event) => {
      const message = event.data;
      if (message.type === "progress") {
        const value = message.total > 0
          ? Math.floor((message.done / message.total) * 100)
          : 0;
        progress.value = value;
        percent.textContent = `${value}%`;
        liveRegion.textContent = `Processing ${message.entry}, ${message.done} of ${message.total} entries.`;
        return;
      }

      if (message.type === "error") {
        finishWithError(message.message);
        return;
      }

      if (message.type === "result") {
        stopWorker();
        setBusy(false);
        showResult(file, message.bytes, message.report);
        return;
      }

      finishWithError("The optimizer returned an unexpected response.");
    });
    worker.addEventListener("error", (errorEvent) => {
      errorEvent.preventDefault();
      const detail = errorEvent.message ? ` ${errorEvent.message}` : "";
      finishWithError(`The optimization worker failed to start. Make sure the page is served over HTTP(S) and that web/worker.js is available.${detail}`);
    });
    worker.addEventListener("messageerror", () => {
      finishWithError("The optimizer returned data that could not be read.");
    });
    worker.postMessage({ type: "optimize", bytes, options: makeOptions() }, [bytes]);
  } catch (error) {
    finishWithError(`Could not read the selected EPUB or start the optimizer: ${errorMessage(error)}`);
  }
});

window.addEventListener("pagehide", () => {
  stopWorker();
  if (downloadUrl !== null) {
    URL.revokeObjectURL(downloadUrl);
  }
});
