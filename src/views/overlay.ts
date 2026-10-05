// Overlays above the start screen and the workspace: the spinner or progress card of a running
// task, and the offer to download ffmpeg, which a video's Meta-Code and Content-Code need, with
// the progress of that download. The modal card and the progress bar serve the Settings dialog too.

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

/** The offer to download ffmpeg before the video at `path` is opened. */
export interface ToolPrompt {
  path: string;
  status: ToolStatus;
  /** Bytes received so far; null until the download starts. */
  download: Download | null;
}

/** Whole percent of a fraction, or a dash while it is unknown. */
function percentDone(fraction: number | null): string {
  return fraction === null ? "…" : `${Math.floor(fraction * 100)}%`;
}

/** Progress bar; without a fraction it moves back and forth. */
export function meter(fraction: number | null): string {
  const fill = fraction === null ? `class="indeterminate"` : `style="width:${(fraction * 100).toFixed(1)}%"`;
  return `<div class="meter" role="progressbar"${fraction === null ? "" : ` aria-valuenow="${Math.floor(fraction * 100)}"`} aria-valuemin="0" aria-valuemax="100"><span ${fill}></span></div>`;
}

/** A modal card with a title, a body and a row of buttons. */
export function dialog(title: string, body: string, buttons: string): string {
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

/** What downloading ffmpeg means, before the user agrees. */
function toolFacts(status: ToolStatus): string {
  const size = status.bytes ? formatBytes(status.bytes) : "unknown";
  const note = status.note ? `<dt>Note</dt><dd>${esc(status.note)}</dd>` : "";
  return `
    <dl class="facts">
      <dt>Download</dt><dd>${esc(size)}, once</dd>
      <dt>From</dt><dd><a class="ext" href="https://github.com/iscc/iscc-binaries/releases">github.com/iscc/iscc-binaries</a></dd>
      <dt>Licence</dt><dd>${esc(status.licence)}, runs as a separate program</dd>
      <dt>Stored in</dt><dd>${esc(status.path ?? "")}</dd>
      ${note}
    </dl>`;
}

/** The offer to download ffmpeg, or its progress once agreed. */
export function toolOverlay(prompt: ToolPrompt): string {
  const { status, download } = prompt;
  if (download) {
    const fraction = download.total ? download.received / download.total : null;
    return dialog(
      "Downloading ffmpeg",
      `${meter(fraction)}<p class="progress-text" data-progress-text>${esc(downloadText(download))}</p>`,
      `<button class="btn small" data-action="cancel-tool">Cancel</button>`,
    );
  }
  return dialog(
    "Video needs ffmpeg",
    `<p>The Content-Code Video comes from MPEG-7 frame signatures, which ffmpeg computes; ffmpeg also reads the tags behind the Meta-Code. Without it, the app shows the video's Content Credentials, Data-Code and Instance-Code only. The app downloads the build iscc-sdk uses and checks it against its BLAKE3 hash before it runs.</p>${toolFacts(status)}`,
    `<button class="btn small quiet" data-action="skip-tool">Open without ffmpeg</button><button class="btn small primary" data-action="install-tool">Download and continue</button>`,
  );
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
