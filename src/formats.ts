// Per-format wording shared by the Sign tab and the Content Credentials tab: how each kind of
// content is hashed, and why removing a manifest leaves traces in some formats.

import type { AssetKind, Inspection } from "./api";

/** How removing a manifest leaves traces in a format's bytes. */
export interface StripCaveat {
  /** Sentence for the note under the soft-binding table. */
  note: string;
  /** Short reason the bitstream units do not survive signing, for the Sign tab. */
  reason: string;
}

const EPUB = "application/epub+zip";

/** Zip containers: c2pa-rs rewrites the central directory, removal leaves the manifest's bytes. */
const ZIP_MIMES = new Set([
  EPUB,
  "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  "application/vnd.openxmlformats-officedocument.presentationml.presentation",
  "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  "application/vnd.oasis.opendocument.text",
  "application/vnd.oasis.opendocument.spreadsheet",
  "application/vnd.oasis.opendocument.presentation",
]);

/** Non-zip formats whose manifest cannot be removed without traces, by MIME type. */
const CAVEATS: Record<string, StripCaveat> = {
  "image/webp": {
    note: "This demo cannot yet remove a manifest from a WebP file, so Data-Code and Instance-Code are computed with the manifest included and do not match.",
    reason: "this demo cannot yet remove a manifest from a WebP file",
  },
  "image/tiff": {
    note: "Embedding a manifest into a TIFF rewrites its header and appends a new image directory that removal does not undo, so Data-Code stays close and Instance-Code does not match.",
    reason: "the image directory the manifest adds to a TIFF stays",
  },
  "image/svg+xml": {
    note: "Removing a manifest from an SVG leaves the namespace declaration it added to the root element, so Data-Code stays close and Instance-Code does not match.",
    reason: "removing the manifest leaves a namespace declaration in the SVG",
  },
  "audio/mpeg": {
    note: "Embedding a manifest into an MP3 rewrites its ID3 tag, and removal does not undo that, so Data-Code stays close and Instance-Code does not match.",
    reason: "embedding the manifest rewrites the ID3 tag, and removal does not restore it",
  },
  "audio/flac": {
    note: "Embedding a manifest into a FLAC file puts an ID3 tag in front of it, and removal leaves that tag empty behind, so Data-Code stays close and Instance-Code does not match.",
    reason: "removing the manifest leaves an empty ID3 tag in front of the FLAC stream",
  },
  "audio/wav": {
    note: "This demo cannot yet remove a manifest from a WAV file, so Data-Code and Instance-Code are computed with the manifest included and do not match.",
    reason: "this demo cannot yet remove a manifest from a WAV file",
  },
};

/** The caveat of the inspected file's format, or null when stripping restores its bytes exactly. */
export function stripCaveat(inspection: Inspection): StripCaveat | null {
  if (!ZIP_MIMES.has(inspection.mime)) return CAVEATS[inspection.mime] ?? null;
  const label = inspection.format_label;
  const whole = inspection.mime === EPUB ? "book" : "document";
  return {
    note: `Removing a manifest from ${label} rewrites its ZIP container, so Data-Code stays close and Instance-Code does not match; Content-Code Text is the unit that identifies the ${whole}.`,
    reason: `removing the manifest rewrites the ${label} container`,
  };
}

/** What the Content-Code of each kind of media is computed from, in one word. */
export const CONTENT_FROM: Record<AssetKind, string> = { image: "pixels", text: "text", audio: "sound" };

/** What the Content-Code of the file is computed from. */
export function contentSource(inspection: Inspection): string {
  if (inspection.kind === "text") return "text";
  if (inspection.kind === "audio") return "decoded audio";
  return inspection.mime === "image/svg+xml" ? "rendered image" : "pixels";
}
