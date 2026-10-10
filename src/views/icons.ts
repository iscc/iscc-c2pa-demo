// Icons drawn with the current text colour: a line icon per kind of media (the start screen's formats table and the
// placeholder of a file without a picture) and the Copy and Check glyphs of the ISCC brand kit's Carbon selection
// (Carbon / IBM, Apache License 2.0, geometry unchanged).

import type { AssetKind } from "../api";

const PATHS: Record<AssetKind, string> = {
  image: `<rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="8.5" cy="8.5" r="1.5"/><path d="M21 15l-5-5L5 21"/>`,
  text: `<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5M9 13h6M9 17h6"/>`,
  audio: `<path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>`,
  video: `<rect x="2" y="5" width="15" height="14" rx="2"/><path d="M17 10l5-3v10l-5-3z"/>`,
};

/** The icon of a kind of media as an inline SVG, hidden from screen readers (a label always sits next to it). */
export function kindIcon(kind: AssetKind): string {
  return `<svg class="kindicon" viewBox="0 0 24 24" aria-hidden="true">${PATHS[kind]}</svg>`;
}

/** Carbon "copy" (brand kit: Copy), for icon-only Copy buttons. */
export const COPY_ICON = `<svg class="glyph" viewBox="0 0 32 32" aria-hidden="true"><path d="M28,10V28H10V10H28m0-2H10a2,2,0,0,0-2,2V28a2,2,0,0,0,2,2H28a2,2,0,0,0,2-2V10a2,2,0,0,0-2-2Z"/><path d="M4,18H2V4A2,2,0,0,1,4,2H18V4H4Z"/></svg>`;

/** Carbon "checkmark--outline" (brand kit: Check), shown for a moment after a successful copy. */
export const CHECK_ICON = `<svg class="glyph" viewBox="0 0 32 32" aria-hidden="true"><polygon points="14 21.414 9 16.413 10.413 15 14 18.586 21.585 11 23 12.415 14 21.414"/><path d="M16,2A14,14,0,1,0,30,16,14,14,0,0,0,16,2Zm0,26A12,12,0,1,1,28,16,12,12,0,0,1,16,28Z"/></svg>`;
