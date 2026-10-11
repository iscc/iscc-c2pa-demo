// Content Credentials found on the ISCC test network for a file without its own: one card per match, its summary row
// (signer, title, verified time, how it matches) expanding to the C2PA verdict for this file with the rebuild check
// and Restore, the claim thumbnail next to this file's picture, this app's ISCC check (IEP-0020), and the manifest's
// details. The wording keeps the two layers apart: C2PA validation decides whether credentials belong to a file; the
// ISCC check only says how the file compares with the soft binding, and never speaks for the signer.

import type { Found, Inspection, InvalidReason, Lookup, ManifestSummary, SoftBindingSummary, TimestampSummary } from "../api";
import { esc, isccHtml, listJoin, percent } from "../util";
import {
  actionsCard,
  assertionsCard,
  type Box,
  ingredientsCard,
  TEST_KEY,
  timestampRow,
  trainingCard,
  trustListName,
  validationCard,
  verdict,
} from "./credentials";
import { HELP_ICON, kindIcon } from "./icons";

/** A part of a found manifest's card, below a thin rule. */
const part: Box = (header, body) => `<div class="part">${header}${body}</div>`;

/** A part's header: the title, and a note at the right. */
function head(title: string, note = ""): string {
  return `<header><h2>${esc(title)}</h2><span class="grow"></span>${note ? `<span class="hint">${esc(note)}</span>` : ""}</header>`;
}

/** The signer as its certificate names it: the common name, else who issued the certificate. */
function signerName(m: ManifestSummary): string {
  const s = m.signature;
  if (s?.common_name) return s.common_name;
  return s?.issuer ? `Unnamed signer (certificate issued by ${s.issuer})` : "Unknown signer";
}

/** What the signer's certificate stands for: a public test key, the trust list it chains to, or none. */
function signerStanding(found: Found): string {
  if (found.manifest.test_key) return TEST_KEY;
  const list = trustListName(found.manifest.trust_list);
  return list ? `On ${list}.` : "Not on a trust list.";
}

/** The signing time, when a timestamp service on the C2PA trust list confirms it; never a time the signer states. */
function verifiedTime(m: ManifestSummary): string | null {
  const ts = m.signature?.timestamp;
  return ts?.status === "trusted" && !ts.legacy ? ts.time : null;
}

/** The day and minute of `iso`, in local format. */
function minute(iso: string): string {
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? iso : new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(date);
}

/** C2PA's word for a validated state, with the signer's standing. */
function validText(state: string, found: Found): string {
  if (found.manifest.test_key) return "Valid, signed with a public test key";
  const list = trustListName(found.manifest.trust_list);
  return state === "Trusted" && list ? `Valid, signer on ${list}` : "Valid, signer not on a trust list";
}

/** The Content-Code similarity as a label part, when there is one. */
function contentText(found: Found): string {
  return found.iscc.content === null ? "" : `Content-Code ${percent(found.iscc.content)}`;
}

/** How a found manifest matches this file, for its summary row: the label and its state. */
function matchLabel(found: Found): [string, string] {
  const rebuild = found.rebuild?.status;
  if (rebuild === "passed" || (found.for_file.status === "valid" && found.iscc.exact)) return ["Exact match", "exact"];
  if (found.for_file.status === "valid") return ["Hash matches this file", "exact"];
  if (found.iscc.exact && rebuild === "failed") return ["Instance-Code equal, rebuild failed", "failed"];
  if (found.iscc.exact && rebuild === "partial") return ["Instance-Code equal, hash covers part", "instance"];
  if (found.iscc.exact) return ["Instance-Code equal", "instance"];
  const content = contentText(found);
  const kind = found.iscc.near ? "Similar" : "Related";
  return [content ? `${kind} · ${content}` : kind, found.iscc.near ? "similar" : "related"];
}

/** The summary row of a found manifest: claim thumbnail, signer, title and verified day, how it matches; it expands
 * the card. The signer comes first, and the title in quotes is the signer's. */
function summaryRow(found: Found, open: boolean, inspection: Inspection): string {
  const m = found.manifest;
  const thumb = m.thumbnail ? `<img src="${esc(m.thumbnail)}" alt="" draggable="false" />` : kindIcon(inspection.kind);
  const time = verifiedTime(m);
  const when = time ? `<span title="${esc(time)}">${esc(minute(time))}, verified</span>` : "no verified time";
  const title = m.title ? `“${esc(m.title)}” · ` : "";
  const tag = m.test_key ? `<span class="tag" title="${esc(TEST_KEY)}">public test key</span>` : "";
  const [label, state] = matchLabel(found);
  return `
    <button type="button" class="match-head" data-expand="${esc(found.store_id)}" aria-expanded="${open}">
      <span class="thumb">${thumb}</span>
      <span class="what">
        <span class="signer"><span class="name">${esc(signerName(m))}</span>${tag}</span>
        <span class="sub">${title}${when}</span>
      </span>
      <span class="label" data-state="${state}"><span class="dot"></span>${esc(label)}</span>
      <span class="chev" aria-hidden="true"></span>
    </button>`;
}

/** A verdict line: its state colours the rule at its left, the title says it, the detail explains it (HTML). */
function verdictBlock(state: string, title: string, detail: string, code = ""): string {
  return `
      <div class="verdict" data-state="${state}">
        <p>${esc(title)}</p>
        <p class="detail">${detail}</p>
        ${code}
      </div>`;
}

/** The failure code of `reason` in mono on a line of its own, with how many more there are; empty without one. */
function codeLine(reason: InvalidReason): string {
  if (!reason.code) return "";
  const more = reason.more > 0 ? ` · +${reason.more} more, see Validation for this file` : "";
  return `<p class="code"><span class="mono">${esc(reason.code)}</span>${esc(more)}</p>`;
}

/** The warning about the signer, which C2PA asks for with any display of credentials that are not valid for a file
 * ("Displaying Manifest Information"), next to the lack of validity. */
const NOT_ATTRIBUTED = "Their content must not be attributed to the signer for this file.";

/** Why credentials found by search are rejected when their hash does not match. */
const MISMATCH_EXPECTED =
  "For credentials found by search this is expected: the hash alone cannot tell whether this file was changed or the credentials belong to other content.";

/** The rebuild check's outcome when it did not pass, as sentences (HTML). */
function rebuildNote(found: Found, mime: string): string {
  const r = found.rebuild;
  if (r?.status === "failed") {
    const code = r.reason.code ? ` (<span class="mono">${esc(r.reason.code)}</span>)` : "";
    const why = r.hash_mismatch ? " So their hash and their ISCC soft binding describe different bytes." : "";
    return ` <strong>Rebuild check failed:</strong> ${esc(r.reason.text)}${code}${why}`;
  }
  if (r?.status === "partial") {
    return ` <strong>Rebuild check:</strong> ${esc(`put back into this file, they validate, but ${uncoveredText(r.uncovered, mime)}, so this app calls it no exact match and offers no Restore.`)}`;
  }
  if (r?.status === "unavailable") return ` Rebuild check not available: ${esc(r.reason)}.`;
  return "";
}

/** What the hash of a rebuilt file leaves out of this file besides the credentials, as a clause. */
function uncoveredText(bytes: number | null, mime: string): string {
  if (bytes === null) return "their hash leaves parts of this file out besides the credentials";
  const why = mime === "audio/mpeg" ? " (c2pa-rs leaves an MP3's whole ID3 tag out of its hash)" : "";
  return `their hash leaves ${bytes} bytes of this file out besides the credentials${why}`;
}

/** The C2PA verdict for this file, of type `mime`: the rebuild check passed (exact match), the hash matches this file
 * as it is, the hash was not judged (box hash), or the credentials are rejected for this file. */
function c2paVerdict(found: Found, mime: string): string {
  const r = found.rebuild;
  const f = found.for_file;
  if (r?.status === "passed") {
    const asItIs = f.status === "valid" ? "" : ` As it is, C2PA rejects them for this file. ${NOT_ATTRIBUTED}`;
    return verdictBlock(
      "exact",
      "Exact match: put back into this file, these credentials validate",
      esc(
        `Rebuild check by this app: with the found manifest store put back, this file validates as the signed file, and their hash covers all of it. C2PA: ${validText(r.state, found)}.${asItIs}`,
      ),
    );
  }
  if (f.status === "valid") {
    return verdictBlock(
      "exact",
      "Their hash matches this file as it is",
      esc(
        `C2PA validates these credentials for this file as it is: they were published apart from exactly these bytes, like a sidecar file. C2PA: ${validText(f.state, found)}.`,
      ),
    );
  }
  if (f.status === "unchecked") {
    return verdictBlock(
      "unchecked",
      "Not validated for this file",
      `${esc(`The C2PA library this app uses (c2pa-rs 0.91) calls them valid for this file, but ${f.reason}, so this app does not rely on that verdict. ${NOT_ATTRIBUTED}`)}${rebuildNote(found, mime)}`,
    );
  }
  const expected = f.hash_mismatch ? ` ${MISMATCH_EXPECTED}` : "";
  return verdictBlock(
    found.rebuild?.status === "failed" ? "failed" : "rejected",
    "Not valid for this file",
    `${esc(`${f.reason.text}${expected} ${NOT_ATTRIBUTED}`)}${rebuildNote(found, mime)}`,
    codeLine(f.reason),
  );
}

/** The button that writes this file with the credentials put back, under an exact match. */
function restoreRow(found: Found): string {
  if (found.rebuild?.status !== "passed") return "";
  return `
      <div class="restore">
        <button type="button" class="btn primary" data-restore="${esc(found.store_id)}">Restore Content Credentials</button>
        <span class="hint">Writes a copy of this file with these credentials put back; this file stays as it is.</span>
      </div>`;
}

/** The claim thumbnail next to this file's picture, to compare by eye; nothing when there is neither. */
function compareBox(m: ManifestSummary, inspection: Inspection): string {
  if (!m.thumbnail && !inspection.preview) return "";
  if (!m.thumbnail) return part(head("Compare by eye"), `<div class="note">These credentials carry no thumbnail to compare.</div>`);
  const current = inspection.preview
    ? `<img src="${inspection.preview}" alt="${esc(inspection.file_name)}" draggable="false" />`
    : `<div class="nopreview">no picture</div>`;
  return part(
    head("Compare by eye"),
    `<div class="thumbs">
        <figure><img src="${esc(m.thumbnail)}" alt="Thumbnail in these credentials" draggable="false" /><figcaption>In these credentials</figcaption></figure>
        <figure>${current}<figcaption>This file</figcaption></figure>
      </div>
      <div class="note">This picture comes with these credentials; it helps to spot the wrong content, but proves nothing.</div>`,
  );
}

/** The ISCC verdict: equal Instance-Code, or how similar the Content-Code is, `near` the lowest near-identical
 * similarity; never in the signer's name. */
function isccVerdict(found: Found, near: number): string {
  const content = contentText(found);
  if (found.iscc.exact) {
    return verdictBlock(
      "exact",
      "Instance-Code equal",
      "This file has exactly the bytes the ISCC soft binding of these credentials was computed from (a BLAKE3 hash of 256 bits). That alone ties none of their other statements to this file; only C2PA validation, above, can.",
    );
  }
  if (found.iscc.near) {
    return verdictBlock(
      "similar",
      `Similar file: ${content}, Instance-Code differs`,
      "Their ISCC soft binding was computed from another file with near-identical content, perhaps the one this file was re-encoded or resized from. What differs is not covered by these credentials.",
    );
  }
  const detail = content
    ? `The Content-Code is below the ${percent(near)} this app requires for near-identical. Their ISCC soft binding was computed from another file; how the two relate is not recorded.`
    : "Their ISCC soft binding was computed from another file and lists no Content-Code this app can compare; how the two relate is not recorded.";
  return verdictBlock("related", `Related content: ${content || "no comparable Content-Code"}`, esc(detail));
}

/** One soft binding's units next to this file's; a Semantic-Code is compared only while the file's is shown. */
function unitTable(sb: SoftBindingSummary, semantic: boolean): string {
  const rows = sb.matches
    .map((mt) => {
      const u = mt.embedded;
      const shown = u.unit === "semantic" && !semantic ? `<span class="hint">not compared</span>` : verdict(mt, false);
      return `
            <tr>
              <td class="unit-name"><span class="swatch" data-unit="${u.unit}"></span>${esc(u.name)}</td>
              <td><span class="mono fit" title="${esc(u.iscc)}">${isccHtml(u.iscc)}</span></td>
              <td class="num">${shown}</td>
            </tr>`;
    })
    .join("");
  return `
          <table class="unit-table">
            <colgroup><col class="name" /><col /><col class="match" /></colgroup>
            <thead><tr><th>Unit</th><th>${sb.scoped ? "For part of the file" : "In these credentials"}</th><th style="text-align:right">This file</th></tr></thead>
            <tbody>${rows}</tbody>
          </table>`;
}

/** This app's ISCC check (IEP-0020): the verdict, each ISCC soft binding's units against this file's, and what keeps
 * the match from being complete or attributed to the signer. */
function isccBox(found: Found, near: number, semantic: boolean): string {
  const tables = found.manifest.soft_bindings
    .filter((sb) => sb.supported && !sb.error)
    .map((sb) => unitTable(sb, semantic))
    .join("");
  const notes = ["Compared with this file's units as the left column shows them."];
  if (!found.attributed) {
    notes.push(
      "These credentials list the soft binding among gathered assertions, which the C2PA specification does not attribute to the signer.",
    );
  }
  if (found.iscc.unchecked.length > 0) {
    notes.push(`Not a complete soft-binding match: ${listJoin(found.iscc.unchecked)} not checked by this app.`);
  }
  return part(
    head("ISCC check by this app", "IEP-0020 draft, not part of C2PA validation"),
    `${isccVerdict(found, near)}${tables}<div class="note">${esc(notes.join(" "))}</div>`,
  );
}

/** The timestamp row of a found manifest: the shared row for a time a service on the C2PA trust list confirms, else
 * why there is no verified time, without the date the signer states. */
function signedAtRow(ts: TimestampSummary | undefined): string {
  if (!ts || ts.status === "none" || ts.status === "rejected" || (ts.status === "trusted" && !ts.legacy)) return timestampRow(ts);
  const why = ts.legacy
    ? "This older (v1) manifest format does not check its timestamp service against the C2PA trust list, so the signing time is not proven."
    : `${ts.tsa ?? "The timestamp service"} is not on the C2PA trust list, so the signing time is not proven.`;
  return `<dt>Timestamp</dt><dd><span class="status" data-ts="none"><span class="dot"></span>No verified time</span><span class="hint">${esc(why)}</span></dd>`;
}

/** Who signed, when, and what the manifest calls itself; the title is the signer's. */
function signerBox(found: Found): string {
  const m = found.manifest;
  const s = m.signature;
  const rows = [
    `<dt>Signer</dt><dd>${esc(signerName(m))}<span class="hint">${esc(signerStanding(found))}</span></dd>`,
    s?.issuer ? `<dt>Certificate issued by</dt><dd>${esc(s.issuer)}</dd>` : "",
    `<dt>Signature</dt><dd>${esc(s?.alg?.toUpperCase() ?? "—")}</dd>`,
    signedAtRow(s?.timestamp),
    `<dt>Title</dt><dd>${m.title ? `“${esc(m.title)}”<span class="hint">Given by the signer.</span>` : "—"}</dd>`,
    `<dt>Claim generator</dt><dd>${esc(m.claim_generator ?? "—")}</dd>`,
    `<dt>Manifest</dt><dd class="mono">${esc(m.label)}</dd>`,
    `<dt>Found on</dt><dd class="mono">${esc(found.hosts.join(", "))}</dd>`,
  ];
  return part(head("Signer and manifest"), `<div class="body"><dl class="kv">${rows.join("")}</dl></div>`);
}

/** What a found manifest's card shows it against: this file, whether its Semantic-Code is shown, and the lowest
 * near-identical similarity. */
interface Against {
  inspection: Inspection;
  semantic: boolean;
  near: number;
}

/** The expanded card of a found manifest: C2PA first, then the picture, the ISCC check and the details. */
function details(found: Found, { inspection, semantic, near }: Against): string {
  const m = found.manifest;
  return `
    <div class="match-body">
      ${part(head("C2PA"), `${c2paVerdict(found, inspection.mime)}${restoreRow(found)}`)}
      ${compareBox(m, inspection)}
      ${isccBox(found, near, semantic)}
      ${signerBox(found)}
      ${trainingCard(m, part)}
      ${actionsCard(m, part)}
      ${ingredientsCard(m, part)}
      ${validationCard(m, part, "Validation for this file")}
      ${assertionsCard(m.assertions, part)}
    </div>`;
}

/** One found manifest as a card; `open` expands it. */
function matchCard(found: Found, open: boolean, against: Against): string {
  return `
    <section class="card match" data-open="${open}">
      ${summaryRow(found, open, against.inspection)}
      ${open ? details(found, against) : ""}
    </section>`;
}

/** How the list is made, behind the help button of its heading. */
const ORDER =
  "Found by this file's ISCC on the ISCC test network. Each manifest store was fetched and checked here; the services' own scores only decide which are fetched. Listed are near-identical matches, those with an equal Instance-Code first, then by earliest verified signing time.";

/** What the search left out and which services did not answer, as one muted paragraph; empty when nothing was. */
function notes(lookup: Lookup): string {
  const lines: string[] = [];
  const reasons = [...new Set(lookup.left_out.map((l) => l.reason))];
  if (reasons.length > 0) {
    const n = new Set(lookup.left_out.map((l) => l.manifest_id)).size;
    lines.push(`Left out (${n === 1 ? "1 manifest" : `${n} manifests`}): ${listJoin(reasons)}.`);
  }
  if (lookup.unchecked > 0) lines.push(`${lookup.unchecked} more not checked.`);
  for (const f of lookup.failures) lines.push(`${f.host} did not answer (${f.reason}).`);
  return lines.length > 0 ? `<p class="search-notes">${esc(lines.join(" "))}</p>` : "";
}

/** The two warnings C2PA asks for when credentials are shown for a file they are not valid for, above the cards;
 * empty when none is listed. */
function warning(listed: Found[]): string {
  if (listed.length === 0) return "";
  const unless = listed.some((f) => f.for_file.status === "valid") ? ", unless a card says their hash matches it" : "";
  return `<p class="found-warning">${esc(`Credentials found by search are not valid for this file as it is${unless}, and their content must not be attributed to their signers for this file. Open one for C2PA's verdict and this app's checks.`)}</p>`;
}

/** What a search found: the near-identical matches as cards (those in `open` expanded), the related ones behind a
 * toggle (`related` shows them), and what was left out. */
export function foundList(lookup: Lookup, open: string[], related: boolean, inspection: Inspection, semantic: boolean): string {
  const against = { inspection, semantic, near: lookup.near_identical };
  const card = (f: Found) => matchCard(f, open.includes(f.store_id), against);
  const n = lookup.matches.length;
  const heading =
    n > 0
      ? `<div class="found-head"><h3>${n === 1 ? "1 match" : `${n} matches`} on the ISCC test network</h3><button type="button" class="tip" aria-label="${esc(ORDER)}" data-tip="${esc(ORDER)}">${HELP_ICON}</button></div>`
      : "";
  const r = lookup.related.length;
  const toggle =
    r > 0
      ? `<button type="button" class="linkbtn related-toggle" data-action="toggle-related">${related ? "Hide" : "Show"} ${r === 1 ? "1 file" : `${r} files`} with related content, not near-identical</button>`
      : "";
  const relatedCards = related ? lookup.related.map(card).join("") : "";
  const listed = related ? [...lookup.matches, ...lookup.related] : lookup.matches;
  return `<div class="found-list">${heading}${warning(listed)}${lookup.matches.map(card).join("")}${toggle}${relatedCards}${notes(lookup)}</div>`;
}
