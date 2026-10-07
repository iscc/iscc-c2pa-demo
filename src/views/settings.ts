// Settings dialog: the experimental Semantic-Codes, switched on and off per kind, and OCR of
// scanned PDF pages. Ticking a Semantic-Code agrees to the download of its model, whose size,
// source and licence the dialog shows; the download runs in the dialog, with its progress and
// Cancel under the box. OCR's models are built in, so its box downloads nothing.

import type { Download, SemanticKind, Switches } from "../api";
import { esc, formatBytes } from "../util";
import { dialog, downloadProgress, meter, patchProgress } from "./overlay";

/** A switch of the dialog: a kind of Semantic-Code, or OCR. */
export type Switch = SemanticKind | "ocr";

/** A switch being changed: its model downloading, or the choice being saved. */
export interface SettingsOperation {
  kind: Switch;
  on: boolean;
  /** Bytes of the model received so far; null while nothing is downloaded. */
  download: Download | null;
  /** True once the user cancelled the download. */
  cancelled: boolean;
}

/** The Settings dialog while it is open. */
export interface SettingsDialog {
  /** The one switch being changed now; null when none is. */
  operation: SettingsOperation | null;
  /** Why the last change of each switch failed. */
  errors: Partial<Record<Switch, string>>;
  /** Why the switches could not be read. */
  error: string | null;
}

/** Each kind of Semantic-Code and the model behind it. */
const KINDS: { kind: SemanticKind; title: string; model: string }[] = [
  { kind: "image", title: "Semantic-Code Image", model: "ISC21 descriptor of iscc-sci" },
  { kind: "text", title: "Semantic-Code Text", model: "Multilingual MiniLM of iscc-sct" },
];

const INTRO =
  "ISCC units that compare what a picture shows or what a text says: they still match after crops, recolouring and " +
  "overlays, or after a translation. Neural networks compute them on this computer; a long book can take a minute. " +
  "Codes may differ from iscc-sci and iscc-sct by a few bits, because the models are compressed.";

const OCR_INTRO =
  "Reads the text of PDF pages that are scans, in Latin script and Chinese, so a scanned document gets a Content-Code " +
  "Text and a Semantic-Code Text. Off, every PDF keeps the codes iscc-sdk computes; on, scans get codes iscc-sdk does " +
  "not compute. A page takes a second or two.";

/** Progress of a model's download, with Cancel. */
function downloadRow(download: Download): string {
  const [fraction, text] = downloadProgress(download);
  return `
      <div class="download">
        ${meter(fraction)}
        <div class="row"><p class="progress-text" data-progress-text>${esc(text)}</p><button type="button" class="btn small" data-action="cancel-settings">Cancel</button></div>
      </div>`;
}

/** The box of one kind: ticked when it is on, or while it is being switched on; disabled while any switch is being
 * changed or a file is being read (`reading`). */
function switchRow(k: (typeof KINDS)[number], settings: Switches | null, open: SettingsDialog, reading: boolean): string {
  const op = open.operation;
  const s = settings?.[k.kind];
  const changing = op?.kind === k.kind ? op : null;
  const checked = changing ? changing.on : Boolean(s?.on);
  const size = s ? (s.installed ? "installed" : `${formatBytes(s.bytes)} download`) : "";
  const error = open.errors[k.kind];
  return `
    <div class="switch">
      <label class="check">
        <input type="checkbox" name="semantic" value="${k.kind}" ${checked ? "checked" : ""} ${op || !settings || reading ? "disabled" : ""} />
        <span class="title"><span class="swatch" data-unit="semantic"></span>${esc(k.title)}</span>
        <span class="desc"${s ? ` title="${esc(s.model)}"` : ""}>${esc(k.model)}${size ? ` · ${esc(size)}` : ""}</span>
      </label>
      ${changing?.download ? downloadRow(changing.download) : ""}
      ${error ? `<p class="failed">${esc(error)}</p>` : ""}
    </div>`;
}

/** The OCR box: ticked when OCR is on, or while it is being switched on; disabled while any switch is being changed
 * or a file is being read (`reading`). */
function ocrRow(settings: Switches | null, open: SettingsDialog, reading: boolean): string {
  const op = open.operation;
  const checked = op?.kind === "ocr" ? op.on : Boolean(settings?.ocr);
  const error = open.errors.ocr;
  return `
    <div class="switch">
      <label class="check">
        <input type="checkbox" name="ocr" value="ocr" ${checked ? "checked" : ""} ${op || !settings || reading ? "disabled" : ""} />
        <span class="title">OCR for scanned PDF pages</span>
        <span class="desc">PP-OCRv6 tiny by PaddlePaddle · Apache-2.0 · built in</span>
      </label>
      ${error ? `<p class="failed">${esc(error)}</p>` : ""}
    </div>`;
}

/** Where the models come from and where they are kept. */
function source(settings: Switches | null): string {
  if (!settings) return "";
  const folder = settings.folder ? ` · stored in <span class="mono">${esc(settings.folder)}</span>` : "";
  return `<p class="source">From <a class="ext" href="${esc(settings.url)}">github.com/iscc/iscc-c2pa-demo</a> · ${esc(settings.licence)} · checked against BLAKE3 hashes${folder}. Switching a Semantic-Code off keeps its model.</p>`;
}

/** The Settings dialog. Done waits while a switch is being changed; the boxes also wait while a file is being read
 * (`reading`), which a switched OCR starts. */
export function settingsOverlay(open: SettingsDialog, settings: Switches | null, reading: boolean): string {
  const body = `
    <div class="settings">
      <div class="legend">Semantic-Codes <span class="hint">experimental</span></div>
      <p>${esc(INTRO)}</p>
      ${KINDS.map((k) => switchRow(k, settings, open, reading)).join("")}
      ${source(settings)}
      <div class="legend">OCR <span class="hint">experimental</span></div>
      <p>${esc(OCR_INTRO)}</p>
      ${ocrRow(settings, open, reading)}
      ${open.error ? `<p class="failed">${esc(open.error)}</p>` : ""}
    </div>`;
  const done = `<button class="btn small primary" data-action="close-settings" ${open.operation ? "disabled" : ""}>Done</button>`;
  return dialog("Settings", body, done);
}

/** Move the download's progress bar and text in place. */
export function patchSettingsDownload(root: HTMLElement, download: Download) {
  const row = root.querySelector<HTMLElement>(".settings .download");
  if (row) patchProgress(row, ...downloadProgress(download));
}
