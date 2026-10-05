// Overlays above the start screen and the workspace: the spinner or progress card of a running
// task, and the offer to download a tool with the progress of that download: ffmpeg, which a
// video's Meta-Code and Content-Code need, or the semantic models behind the Semantic-Codes.

import type { Download, ToolStatus } from "../api";
import { esc, formatBytes } from "../util";

/** A task the UI waits for. */
export interface Busy {
  /** What runs: the spinner's label, the progress card's title. */
  label: string;
  /** Share done, once a video analysis reports it; null while unknown. */
  fraction: number | null;
  /** True once the task reports progress: a progress card then replaces the spinner. */
  reporting: boolean;
  /** Whether the progress card offers Cancel. */
  cancellable: boolean;
}

/** The offer to download a tool: ffmpeg before the video at `path` is opened, or the semantic models for the file
 * open now (`path` null). */
export interface ToolPrompt {
  tool: "ffmpeg" | "semantic";
  path: string | null;
  status: ToolStatus;
  /** Bytes received so far; null until the download starts. */
  download: Download | null;
}

/** Whole percent of a fraction, or a dash while it is unknown. */
function percentDone(fraction: number | null): string {
  return fraction === null ? "…" : `${Math.floor(fraction * 100)}%`;
}

/** Progress bar; without a fraction it moves back and forth. */
function meter(fraction: number | null): string {
  const fill = fraction === null ? `class="indeterminate"` : `style="width:${(fraction * 100).toFixed(1)}%"`;
  return `<div class="meter" role="progressbar"${fraction === null ? "" : ` aria-valuenow="${Math.floor(fraction * 100)}"`} aria-valuemin="0" aria-valuemax="100"><span ${fill}></span></div>`;
}

/** A modal card with a title, a body and a row of buttons. */
function dialog(title: string, body: string, buttons: string): string {
  return `
    <div class="overlay">
      <section class="card dialog" role="dialog" aria-modal="true" aria-labelledby="dialog-title">
        <header><h2 id="dialog-title">${esc(title)}</h2></header>
        <div class="body">${body}${buttons ? `<div class="buttons">${buttons}</div>` : ""}</div>
      </section>
    </div>`;
}

/** Spinner for a short task; progress card with Cancel for a video analysis. */
export function busyOverlay(busy: Busy): string {
  if (!busy.reporting) {
    return `<div class="busy"><div class="ring" role="status" aria-label="${esc(busy.label)}"></div></div>`;
  }
  const cancel = busy.cancellable ? `<button class="btn small" data-action="cancel-task">Cancel</button>` : "";
  return dialog(busy.label, `${meter(busy.fraction)}<p class="progress-text" data-progress-text>${percentDone(busy.fraction)}</p>`, cancel);
}

/** Text under the download's progress bar. */
function downloadText(download: Download): string {
  return `${formatBytes(download.received)} of ${formatBytes(download.total)}`;
}

/** Where each tool comes from, and what its licence means for the app. */
const SOURCES: Record<ToolPrompt["tool"], { href: string; text: string; licence: string }> = {
  ffmpeg: {
    href: "https://github.com/iscc/iscc-binaries/releases",
    text: "github.com/iscc/iscc-binaries",
    licence: "runs as a separate program",
  },
  semantic: {
    href: "https://github.com/iscc/iscc-c2pa-demo/releases/tag/models-v1",
    text: "github.com/iscc/iscc-c2pa-demo",
    licence: "run inside the app, offline",
  },
};

/** What downloading a tool means, before the user agrees. */
function toolFacts(prompt: ToolPrompt): string {
  const { status } = prompt;
  const source = SOURCES[prompt.tool];
  const size = status.bytes ? formatBytes(status.bytes) : "unknown";
  const note = status.note ? `<dt>Note</dt><dd>${esc(status.note)}</dd>` : "";
  return `
    <dl class="facts">
      <dt>Download</dt><dd>${esc(size)}, once</dd>
      <dt>From</dt><dd><a class="ext" href="${esc(source.href)}">${esc(source.text)}</a></dd>
      <dt>Licence</dt><dd>${esc(status.licence)}, ${esc(source.licence)}</dd>
      <dt>Stored in</dt><dd>${esc(status.path ?? "")}</dd>
      ${note}
    </dl>`;
}

/** Title, explanation and buttons of the offer to download each tool. */
const OFFERS: Record<ToolPrompt["tool"], { title: string; text: string; buttons: string }> = {
  ffmpeg: {
    title: "Video needs ffmpeg",
    text: "The Content-Code Video comes from MPEG-7 frame signatures, which ffmpeg computes; ffmpeg also reads the tags behind the Meta-Code. Without it, the app shows the video's Content Credentials, Data-Code and Instance-Code only. The app downloads the build iscc-sdk uses and checks it against its BLAKE3 hash before it runs.",
    buttons: `<button class="btn small quiet" data-action="skip-tool">Open without ffmpeg</button><button class="btn small primary" data-action="install-tool">Download and continue</button>`,
  },
  semantic: {
    title: "Semantic-Codes need two models",
    text: "The Semantic-Codes are experimental ISCC units that compare what a picture shows or what a text says: they still match after crops, recolouring and overlays, or after a translation. Two neural networks compute them on this computer: the ISC21 image descriptor of iscc-sci and the multilingual MiniLM of iscc-sct, compressed for this app, so codes may differ from iscc-sci and iscc-sct by a few bits. The app checks both against their BLAKE3 hashes.",
    buttons: `<button class="btn small quiet" data-action="dismiss-tool">Not now</button><button class="btn small primary" data-action="install-tool">Download</button>`,
  },
};

/** The offer to download a tool, or its progress once agreed. */
export function toolOverlay(prompt: ToolPrompt): string {
  const { download } = prompt;
  if (download) {
    const fraction = download.total ? download.received / download.total : null;
    return dialog(
      prompt.tool === "ffmpeg" ? "Downloading ffmpeg" : "Downloading the semantic models",
      `${meter(fraction)}<p class="progress-text" data-progress-text>${esc(downloadText(download))}</p>`,
      `<button class="btn small" data-action="cancel-tool">Cancel</button>`,
    );
  }
  const offer = OFFERS[prompt.tool];
  return dialog(offer.title, `<p>${esc(offer.text)}</p>${toolFacts(prompt)}`, offer.buttons);
}

/** Move the progress bar and its text in place, so frequent updates need no full render. */
export function patchProgress(root: HTMLElement, fraction: number | null, text?: string) {
  const fill = root.querySelector<HTMLElement>(".meter span");
  const label = root.querySelector<HTMLElement>("[data-progress-text]");
  if (fill && fraction !== null) {
    fill.classList.remove("indeterminate");
    fill.style.width = `${(fraction * 100).toFixed(1)}%`;
  }
  if (label) label.textContent = text ?? percentDone(fraction);
}

/** Text of a download's progress, for `patchProgress`. */
export function downloadProgress(download: Download): [number | null, string] {
  return [download.total ? download.received / download.total : null, downloadText(download)];
}
