// Left column: preview (image, document cover or thumbnail, audio cover art), file facts and the
// ISCC units computed from the file.

import type { AssetKind, Inspection, IsccUnit, MetaFields } from "../api";
import { esc, formatBytes, formatDuration, isccHtml } from "../util";

/** Placeholder for a file without a picture: EPUBs lack a cover, audio lacks cover art, other
 * documents show their format. */
function placeholder(inspection: Inspection): string {
  let text = inspection.format_label;
  if (inspection.mime === "application/epub+zip") text = "no cover image";
  if (inspection.kind === "audio") text = "no cover art";
  return `<div class="nopreview">${esc(text)}</div>`;
}

/** Pixel size of an image (the rendered size of an SVG), the extracted text length ("none"
 * when there is no text to count: the unit list says why) or the audio duration. */
function extent(inspection: Inspection): string {
  if (inspection.kind === "text") {
    const text = inspection.characters === null ? "none" : `${inspection.characters.toLocaleString()} characters`;
    return `<dt>Text</dt><dd>${text}</dd>`;
  }
  if (inspection.kind === "audio") return `<dt>Duration</dt><dd>${formatDuration(inspection.duration_secs ?? 0)}</dd>`;
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
      <div class="preview${inspection.kind === "image" ? "" : " document"}">${preview}</div>
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

/** Row of a computed unit; the Meta-Code row says where its title came from. */
function unitRow(u: IsccUnit, meta: MetaFields): string {
  return `
      <div class="unit">
        <span class="bar" data-unit="${u.unit}"></span>
        <div>
          <div class="name">${esc(u.name)}</div>
          <div class="code">${isccHtml(u.iscc)}</div>
          ${u.unit === "meta" ? `<div class="from">${esc(metaFrom(meta))}</div>` : ""}
        </div>
        <button class="btn small quiet" data-copy="${esc(u.iscc)}" title="Copy">Copy</button>
      </div>`;
}

/** List of the file's ISCC units with their unit colour; a Meta-Code or Content-Code that could
 * not be computed keeps its place and says why. */
export function unitList(inspection: Inspection, hint: string): string {
  const meta = inspection.meta_fields;
  const rows = inspection.iscc.map((u) => unitRow(u, meta));
  if (inspection.content_error) {
    const row = missingUnitRow("content", CONTENT_NAME[inspection.kind], inspection.content_error, "");
    rows.splice(inspection.meta_error ? 0 : 1, 0, row);
  }
  if (inspection.meta_error) rows.unshift(missingUnitRow("meta", "Meta-Code", inspection.meta_error, metaFrom(meta)));
  return `
    <section class="card">
      <header><h2>ISCC of this file</h2><span class="grow"></span><span class="hint">${esc(hint)}</span></header>
      <div class="units">${rows.join("")}</div>
    </section>`;
}
