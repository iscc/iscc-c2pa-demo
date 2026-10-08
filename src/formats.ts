// Per-format wording shared by the Sign tab and the Content Credentials tab: what the Content-Code
// is computed from, and what embedding a manifest does to a format's bytes (IEP-0020, Source
// Preservation).

import type { AssetKind, Inspection } from "./api";

/** What embedding a manifest does to a format's bytes, as measured with c2pa-rs 0.91 and, for PDF, with the update
 * section this app appends (test `source_preservation_per_format`). `why` completes "Signing ..." for formats that are
 * not source-preserving. */
export type Embedding = { kind: "preserved" } | { kind: "changed" | "no_source_view"; why: string };

/** Formats whose embedding changes bytes outside the manifest store, by MIME type. */
const CHANGED: Record<string, string> = {
  "image/webp": "a WebP rewrites its RIFF size field",
  "image/tiff": "a TIFF adds an image directory",
  "image/svg+xml": "an SVG adds a namespace declaration",
  "audio/mpeg": "an MP3 rewrites its ID3 tag",
  "audio/flac": "a FLAC file puts an ID3 tag in front of the audio",
  "audio/wav": "a WAV file rewrites its RIFF size field",
  "video/x-msvideo": "an AVI file rewrites its RIFF size field",
};

/** Zip containers: C2PA hashes them entry by entry. */
const ZIP_MIMES = new Set([
  "application/epub+zip",
  "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  "application/vnd.openxmlformats-officedocument.presentationml.presentation",
  "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  "application/vnd.oasis.opendocument.text",
  "application/vnd.oasis.opendocument.spreadsheet",
  "application/vnd.oasis.opendocument.presentation",
]);

/** MP4 and QuickTime containers (ISO BMFF), which C2PA hashes box by box, and what they are called. */
const BMFF: Record<string, string> = {
  "audio/mp4": "an MP4",
  "video/mp4": "an MP4",
  "video/x-m4v": "an MP4",
  "video/quicktime": "a QuickTime",
};

/** What signing a file of this format does to its bytes. */
export function embedding(inspection: Inspection): Embedding {
  const { mime, format_label: label } = inspection;
  if (ZIP_MIMES.has(mime)) return { kind: "no_source_view", why: `${label} is hashed entry by entry, as a ZIP container` };
  if (BMFF[mime]) return { kind: "no_source_view", why: `${label} is hashed box by box, as ${BMFF[mime]} container` };
  const changed = CHANGED[mime];
  return changed ? { kind: "changed", why: changed } : { kind: "preserved" };
}

/** What the Content-Code of each kind of media is computed from, in one word. */
export const CONTENT_FROM: Record<AssetKind, string> = { image: "pixels", text: "text", audio: "sound", video: "frames" };

/** What the Content-Code of the file is computed from. */
export function contentSource(inspection: Inspection): string {
  if (inspection.kind === "text") return inspection.ocr?.on ? "text, scanned pages recognised by OCR" : "text";
  if (inspection.kind === "audio") return "decoded audio";
  if (inspection.kind === "video") return "video frames";
  return inspection.mime === "image/svg+xml" ? "rendered image" : "pixels";
}
