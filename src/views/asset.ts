// Left column: preview (image, document cover or thumbnail, audio cover art, video frame), file
// facts and the ISCC units computed from the file, with the progress of those still being computed.

import type { AssetKind, Inspection, IsccUnit, MetaFields, OcrPages, UnitSlug } from "../api";
import { esc, formatBytes, formatDuration, isccHtml } from "../util";

/** Placeholder for a file without a picture: EPUBs lack a cover, audio lacks cover art, a video
 * lacks a preview frame (no video stream, no ffmpeg, or no frame ffmpeg could pick; the unit list
 * says why when the Content-Code is missing too), other documents show their format. */
function placeholder(inspection: Inspection): string {
  let text = inspection.format_label;
  if (inspection.mime === "application/epub+zip") text = "no cover image";
  if (inspection.kind === "audio") text = "no cover art";
  if (inspection.kind === "video") text = "no preview frame";
  return `<div class="nopreview">${esc(text)}</div>`;
}

/** Pixel size of an image (the rendered size of an SVG), the extracted text length ("none"
 * when there is no text to count: the unit list says why), the audio duration, or the duration
 * and frame size of a video. */
function extent(inspection: Inspection): string {
  if (inspection.kind === "text") {
    const text = inspection.characters === null ? "none" : `${inspection.characters.toLocaleString()} characters`;
    return `<dt>Text</dt><dd>${text}</dd>`;
  }
  const duration = `<dt>Duration</dt><dd>${inspection.duration_secs === null ? "unknown" : formatDuration(inspection.duration_secs)}</dd>`;
  if (inspection.kind === "audio") return duration;
  if (inspection.kind === "video") {
    const frames = inspection.width ? `<dt>Frames</dt><dd>${inspection.width} × ${inspection.height}</dd>` : "";
    return duration + frames;
  }
  const label = inspection.mime === "image/svg+xml" ? "Rendered" : "Pixels";
  return `<dt>${label}</dt><dd>${inspection.width} × ${inspection.height}</dd>`;
}

/** Preview card with file facts. Document pictures and cover art keep their natural size. */
export function assetCard(inspection: Inspection): string {
  const preview = inspection.preview
    ? `<img src="${inspection.preview}" alt="${esc(inspection.file_name)}" draggable="false" />`
    : placeholder(inspection);
  const creator = inspection.creator ? `<dt>Creator</dt><dd>${esc(inspection.creator)}</dd>` : "";
  return `
    <section class="card">
      <div class="preview${inspection.kind === "image" || inspection.kind === "video" ? "" : " document"}">${preview}</div>
      <div class="body">
        <dl class="facts">
          <dt>File</dt><dd>${esc(inspection.file_name)}</dd>
          <dt>Format</dt><dd title="${esc(inspection.mime)}">${esc(inspection.format_label)}</dd>
          ${extent(inspection)}
          ${creator}
          <dt>Size</dt><dd>${esc(formatBytes(inspection.size_bytes))}</dd>
        </dl>
      </div>
    </section>`;
}

const NAME_SOURCE: Record<MetaFields["name_source"], string> = {
  metadata: "the file's metadata",
  manifest: "the Content Credentials",
  filename: "the file name",
};

/** Where the Meta-Code's title (and description) came from. */
function metaFrom(fields: MetaFields): string {
  const extra = fields.meta ? " and ISCC metadata" : fields.description ? " and description" : "";
  return `Title “${fields.name}”${extra}, from ${NAME_SOURCE[fields.name_source]}`;
}

/** Name of the Content-Code of each kind of asset. */
const CONTENT_NAME: Record<AssetKind, string> = {
  image: "Content-Code Image",
  text: "Content-Code Text",
  audio: "Content-Code Audio",
  video: "Content-Code Video",
};

/** Name of the Semantic-Code of an image or a text; audio and video have none. */
export function semanticName(kind: AssetKind): string {
  return kind === "image" ? "Semantic-Code Image" : "Semantic-Code Text";
}

/** Units in the order of their MainType, as the backend lists them. */
const ORDER: UnitSlug[] = ["meta", "semantic", "content", "data", "instance"];

/** The background pass that computes a file's slow units, as far as the unit list shows it. */
export interface Analysis {
  /** Share done of each slow unit that reports it (a video's or a scan's `content`, a text's `semantic`); null
   * while unknown. */
  progress: Partial<Record<UnitSlug, number | null>>;
  /** True once the user stopped it. */
  stopped: boolean;
}

/** What the unit list needs besides the inspection. */
export interface UnitListView {
  /** Which bytes the units were computed from. */
  hint: string;
  analysis: Analysis | null;
  /** Whether the file's Semantic-Code is shown: its kind is on and the inspection mentions it. */
  semantic: boolean;
}

/** Name of the unit `slug` for this file. */
function unitName(slug: UnitSlug, inspection: Inspection): string {
  const names: Record<UnitSlug, string> = {
    meta: "Meta-Code",
    semantic: semanticName(inspection.kind),
    content: CONTENT_NAME[inspection.kind],
    data: "Data-Code",
    instance: "Instance-Code",
  };
  return names[slug];
}

/** Why the unit `slug` is missing; null when there is no reason (a Semantic-Code of audio or video). */
function unitError(slug: UnitSlug, inspection: Inspection): string | null {
  if (slug === "meta") return inspection.meta_error;
  if (slug === "semantic") return inspection.semantic_error;
  return slug === "content" ? inspection.content_error : null;
}

/** A unit's name, tagged experimental for the Semantic-Code. */
function nameHtml(unit: UnitSlug, name: string): string {
  return `${esc(name)}${unit === "semantic" ? ` <span class="hint">experimental</span>` : ""}`;
}

/** Row for a unit that could not be computed, with the reason, a way to get it (`action`, HTML) and, for the
 * Meta-Code, the inputs it was tried on. */
function missingUnitRow(unit: UnitSlug, name: string, error: string, from: string, action = ""): string {
  return `
      <div class="unit">
        <span class="bar" data-unit="${unit}"></span>
        <div>
          <div class="name">${nameHtml(unit, name)}</div>
          <div class="code" style="color:var(--muted)">not computed: ${esc(error)}${action}</div>
          ${from ? `<div class="from">${esc(from)}</div>` : ""}
        </div>
      </div>`;
}

/** Whole percent of a share, or nothing while it is unknown. */
function percentOf(fraction: number | null | undefined): string {
  return typeof fraction === "number" ? ` ${Math.floor(fraction * 100)}%` : "";
}

/** What a unit still being computed is waiting for: the recognition of a scan's pages, else just computing. */
function pendingLabel(slug: UnitSlug, inspection: Inspection): string {
  const ocr = inspection.ocr;
  if (slug !== "content" || !ocr?.on) return "computing…";
  return `recognising text on ${ocr.scanned} scanned ${ocr.scanned === 1 ? "page" : "pages"}…`;
}

/** Row of a unit the background pass still computes, with its progress (`label` says what it waits for); once the
 * pass is stopped, a way to resume it. */
function pendingRow(slug: UnitSlug, name: string, label: string, analysis: Analysis | null): string {
  const fraction = analysis?.progress[slug];
  const status = analysis?.stopped
    ? `stopped · <button type="button" class="linkbtn" data-action="resume-analysis">Resume</button>`
    : `<span data-pending-text="${slug}" data-label="${esc(label)}">${esc(label)}${percentOf(fraction)}</span>`;
  const fill = typeof fraction === "number" ? `style="width:${(fraction * 100).toFixed(1)}%"` : `class="indeterminate"`;
  const meter = analysis?.stopped ? "" : `<div class="meter" data-pending="${slug}"><span ${fill}></span></div>`;
  return `
      <div class="unit">
        <span class="bar" data-unit="${slug}"></span>
        <div>
          <div class="name">${nameHtml(slug, name)}</div>
          <div class="code" style="color:var(--muted)">${status}</div>
          ${meter}
        </div>
      </div>`;
}

/** Where a signed video copy's Content-Code came from. */
const FROM_SOURCE = "From the file just signed, whose video this copy carries unchanged, packet for packet";

/** Row of a computed unit, with where its inputs came from when that needs saying (`from`). */
function unitRow(u: IsccUnit, from: string): string {
  return `
      <div class="unit">
        <span class="bar" data-unit="${u.unit}"></span>
        <div>
          <div class="name">${nameHtml(u.unit, u.name)}</div>
          <div class="code">${isccHtml(u.iscc)}</div>
          ${from ? `<div class="from">${esc(from)}</div>` : ""}
        </div>
        <button class="btn small quiet" data-copy="${esc(u.iscc)}" title="Copy">Copy</button>
      </div>`;
}

/** Which pages of a scan OCR read for the Content-Code. */
function ocrFrom(ocr: OcrPages): string {
  if (ocr.scanned < ocr.pages) return `Text of ${ocr.scanned} of ${ocr.pages} pages recognised by OCR`;
  return ocr.pages === 1 ? "Text recognised by OCR" : `Text of all ${ocr.pages} pages recognised by OCR`;
}

/** Where a unit's inputs came from: the Meta-Code's title, a signed video copy's Content-Code, a scan's text. */
function unitFrom(u: IsccUnit, inspection: Inspection): string {
  if (u.unit === "meta") return metaFrom(inspection.meta_fields);
  if (u.unit !== "content") return "";
  if (inspection.ocr?.on) return ocrFrom(inspection.ocr);
  return inspection.content_from_source ? FROM_SOURCE : "";
}

/** The way to a Content-Code that OCR would give: a scan without a text layer while OCR is off. */
function ocrAction(slug: UnitSlug, inspection: Inspection): string {
  if (slug !== "content" || !inspection.ocr || inspection.ocr.on) return "";
  return ` · <button type="button" class="linkbtn" data-action="settings">Turn on OCR in Settings</button>`;
}

/** The row of unit `slug`: the unit, its progress while a later pass computes it, or why it is missing; empty when
 * the file has no such unit, or for a Semantic-Code that is not shown. A Meta-Code missing for the Content-Code's
 * reason was never tried (a video's tags unread without ffmpeg), so it names no inputs. */
function row(slug: UnitSlug, inspection: Inspection, view: UnitListView): string {
  if (slug === "semantic" && !view.semantic) return "";
  const unit = inspection.iscc.find((u) => u.unit === slug);
  if (unit) return unitRow(unit, unitFrom(unit, inspection));
  const name = unitName(slug, inspection);
  if (inspection.pending.includes(slug)) return pendingRow(slug, name, pendingLabel(slug, inspection), view.analysis);
  const error = unitError(slug, inspection);
  if (!error) return "";
  const from = slug === "meta" && error !== inspection.content_error ? metaFrom(inspection.meta_fields) : "";
  return missingUnitRow(slug, name, error, from, ocrAction(slug, inspection));
}

/** List of the file's ISCC units with their unit colour; a unit that could not be computed keeps its place and says
 * why, one still being computed shows its progress. */
export function unitList(inspection: Inspection, view: UnitListView): string {
  const rows = ORDER.map((slug) => row(slug, inspection, view)).join("");
  const stop = view.analysis && !view.analysis.stopped ? `<button class="btn small quiet" data-action="stop-analysis">Stop</button>` : "";
  return `
    <section class="card">
      <header><h2>ISCC of this file</h2><span class="grow"></span><span class="hint">${esc(view.hint)}</span>${stop}</header>
      <div class="units">${rows}</div>
    </section>`;
}

/** Move the progress of the pending unit `slug` in place, so frequent reports need no full render. */
export function patchPending(root: HTMLElement, slug: UnitSlug, fraction: number | null) {
  const fill = root.querySelector<HTMLElement>(`[data-pending="${slug}"] span`);
  const text = root.querySelector<HTMLElement>(`[data-pending-text="${slug}"]`);
  if (fill && fraction !== null) {
    fill.classList.remove("indeterminate");
    fill.style.width = `${(fraction * 100).toFixed(1)}%`;
  }
  if (text) text.textContent = `${text.dataset.label ?? "computing…"}${percentOf(fraction)}`;
}
