// Left column: preview (image, document cover or thumbnail, audio cover art, video frame), file
// facts and the ISCC units computed from the file.

import type { AssetKind, Inspection, IsccUnit, MetaFields } from "../api";
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

/** Row for a unit that could not be computed, with the reason and, for the Meta-Code, the inputs
 * it was tried on. */
function missingUnitRow(unit: IsccUnit["unit"], name: string, error: string, from: string): string {
  return `
      <div class="unit">
        <span class="bar" data-unit="${unit}"></span>
        <div>
          <div class="name">${esc(name)}</div>
          <div class="code" style="color:var(--muted)">not computed: ${esc(error)}</div>
          ${from ? `<div class="from">${esc(from)}</div>` : ""}
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
          <div class="name">${esc(u.name)}</div>
          <div class="code">${isccHtml(u.iscc)}</div>
          ${from ? `<div class="from">${esc(from)}</div>` : ""}
        </div>
        <button class="btn small quiet" data-copy="${esc(u.iscc)}" title="Copy">Copy</button>
      </div>`;
}

/** Where a unit's inputs came from: the Meta-Code's title, a signed video copy's Content-Code. */
function unitFrom(u: IsccUnit, inspection: Inspection): string {
  if (u.unit === "meta") return metaFrom(inspection.meta_fields);
  return u.unit === "content" && inspection.content_from_source ? FROM_SOURCE : "";
}

/** List of the file's ISCC units with their unit colour; a Meta-Code or Content-Code that could
 * not be computed keeps its place and says why. A Meta-Code missing for the Content-Code's reason
 * was never tried (a video's tags unread without ffmpeg), so it names no inputs. */
export function unitList(inspection: Inspection, hint: string): string {
  const { meta_error, content_error } = inspection;
  const rows = inspection.iscc.map((u) => unitRow(u, unitFrom(u, inspection)));
  if (content_error) {
    const row = missingUnitRow("content", CONTENT_NAME[inspection.kind], content_error, "");
    rows.splice(meta_error ? 0 : 1, 0, row);
  }
  if (meta_error) {
    const from = meta_error === content_error ? "" : metaFrom(inspection.meta_fields);
    rows.unshift(missingUnitRow("meta", "Meta-Code", meta_error, from));
  }
  return `
    <section class="card">
      <header><h2>ISCC of this file</h2><span class="grow"></span><span class="hint">${esc(hint)}</span></header>
      <div class="units">${rows.join("")}</div>
    </section>`;
}
