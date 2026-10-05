// Start screen: the drop zone with the supported formats and the ISCC units each kind of media
// gets, all of them carried by one C2PA soft binding.

import type { AppInfo, AssetKind, KindInfo } from "../api";
import crLogoUrl from "../assets/content_credentials_logo.svg";
import { CONTENT_FROM } from "../formats";
import { esc } from "../util";

/** Line icons for each kind of media, drawn with the current text colour. */
const ICONS: Record<AssetKind, string> = {
  image: `<rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="8.5" cy="8.5" r="1.5"/><path d="M21 15l-5-5L5 21"/>`,
  text: `<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5M9 13h6M9 17h6"/>`,
  audio: `<path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>`,
  video: `<rect x="2" y="5" width="15" height="14" rx="2"/><path d="M17 10l5-3v10l-5-3z"/>`,
};

/** The units of each row, as [unit, what it is computed from]; the Content-Code varies by kind, and only images and
 * documents have a Semantic-Code (empty for the others). */
function units(kind: AssetKind): [string, string][] {
  const semantic = kind === "image" || kind === "text" ? "meaning" : "";
  return [
    ["meta", "metadata"],
    ["semantic", semantic],
    ["content", CONTENT_FROM[kind]],
    ["data", "bytes"],
    ["instance", "checksum"],
  ];
}

/** One row of the formats table: kind of media, its formats and its units. */
function kindRow(k: KindInfo): string {
  const icon = `<svg viewBox="0 0 24 24" aria-hidden="true">${ICONS[k.kind]}</svg>`;
  const chips = k.formats.map((f) => `<span class="format">${esc(f)}</span>`).join("");
  const cells = units(k.kind)
    .map(([unit, from]) => `<span class="cell">${from ? `<span data-unit="${unit}">${esc(from)}</span>` : ""}</span>`)
    .join("");
  return `<div class="media">${icon}${esc(k.label)}</div><div class="chips">${chips}</div>${cells}`;
}

/** Table of supported formats, with a bracket under the units that one soft binding carries. The Semantic-Code is
 * experimental; while both kinds are off (`semanticOff`), a line points to Settings. */
function formatsTable(kinds: KindInfo[], semanticOff: boolean): string {
  if (!kinds.length) return "";
  const head = ["Media", "Formats", "Meta", "Semantic", "Content", "Data", "Instance"]
    .map((h) => `<span class="head">${h}</span>`)
    .join("");
  const models = semanticOff
    ? `<div class="models">Semantic-Codes are experimental and off. <button type="button" class="linkbtn" data-action="settings">Turn them on in Settings</button>.</div>`
    : "";
  return `
    <div class="kinds">
      ${head}
      ${kinds.map(kindRow).join("")}
      <div class="binding"><span class="bracket"></span>one C2PA soft binding</div>
      ${models}
    </div>`;
}

/** Footer with the standards the demo implements and the Content Credentials mark. */
function footer(info: AppInfo | null): string {
  const alg = info ? ` · <span>C2PA soft-binding algorithm ${esc(info.soft_binding_alg)}</span>` : "";
  return `
    <footer class="startfoot">
      <span><a class="ext" href="https://www.iso.org/standard/77899.html">ISO 24138:2024</a>${alg} · <a class="ext" href="https://ieps.iscc.codes/iep-0020/">IEP-0020</a></span>
      <span class="spacer"></span>
      <span class="cr">Inspects and creates <img src="${crLogoUrl}" alt="Content Credentials" draggable="false" /></span>
    </footer>`;
}

/** The start screen shown while no file is open; `error` is the last failure to open one, `semanticOff` whether both
 * kinds of Semantic-Code are off. */
export function startScreen(info: AppInfo | null, error: string | null, semanticOff: boolean): string {
  return `
    <div class="empty">
      <div class="dropzone" data-action="open" role="button" tabindex="0">
        <div class="ring"><div class="dot"></div></div>
        <h1>Drop an image, a document, an audio or a video file</h1>
        <p>See its Content Credentials and ISCC, or sign it with an ISCC soft binding.</p>
        ${formatsTable(info?.kinds ?? [], semanticOff)}
        ${error ? `<p class="banner error">${esc(error)}</p>` : ""}
      </div>
      ${footer(info)}
    </div>`;
}
