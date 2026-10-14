// Content Credentials found on the ISCC test network for a file without its own: one card per match, its summary row
// (signer, title, verified time, how it matches) expanding to the C2PA verdict for this file with the rebuild check
// and Restore, the claim thumbnail next to this file's picture, this app's ISCC check (IEP-0020), and the manifest's
// details. The wording keeps the two layers apart: C2PA validation decides whether credentials belong to a file; the
// ISCC check only says how the file compares with the soft binding, and never speaks for the signer.

import type { Found, Inspection, InvalidReason, Lookup, ManifestSummary, SoftBindingSummary, TimestampSummary, UnitMatch } from "../api";
import { type ScoredUnit, type Scores, SOFT, TIERS, type Tier, tier, tierRank, UNRANKED, WEAK } from "../tiers";
import { esc, listJoin, percent } from "../util";
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
} from "./credentials";
import { helpTip, kindIcon } from "./icons";

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

/** Short names of the units a kind of match is read from. */
const SHORT_NAME: Partial<Record<ScoredUnit, string>> = { meta: "Meta", semantic: "Semantic", content: "Content", data: "Data" };

/** The unit of `mt` when it counts for the kind of match: compared with this file's (a Semantic-Code only while this
 * file's is shown), and not the Instance-Code, which counts only as equal or not. */
function scoredUnit(mt: UnitMatch, semantic: boolean): ScoredUnit | null {
  const unit = mt.embedded.unit as ScoredUnit;
  if (mt.similarity === null || !(unit in SHORT_NAME)) return null;
  return unit === "semantic" && !semantic ? null : unit;
}

/** The similarity of each unit of the ISCC soft binding to this file's, the lowest where several blocks list one, and
 * the Instance-Code as 1 when equal. */
function unitScores(found: Found, semantic: boolean): Scores {
  const scores: Scores = found.iscc.exact ? { instance: 1 } : {};
  for (const sb of found.manifest.soft_bindings.filter((b) => b.supported && !b.error && !b.scoped)) {
    for (const mt of sb.matches) {
      const unit = scoredUnit(mt, semantic);
      if (unit) scores[unit] = Math.min(scores[unit] ?? 1, mt.similarity ?? 0);
    }
  }
  return scores;
}

/** `units` with their similarity, as a match's evidence: "Semantic 95.7% · Content 82%". */
function unitList(scores: Scores, units: ScoredUnit[]): string {
  return units.map((u) => `${SHORT_NAME[u]} ${percent(scores[u] ?? 0)}`).join(" · ");
}

/** The units a kind of match rests on, those at 60% or more; the closest one when no unit is. */
function evidence(scores: Scores): string {
  const listed = (Object.keys(SHORT_NAME) as ScoredUnit[]).filter((u) => scores[u] !== undefined);
  const backing = listed.filter((u) => (scores[u] ?? 0) >= WEAK);
  const closest = [...listed].sort((a, b) => (scores[b] ?? 0) - (scores[a] ?? 0)).slice(0, 1);
  return unitList(scores, backing.length > 0 ? backing : closest);
}

/** The Content-Code and Data-Code where they are below 70%, which make an equal Instance-Code incoherent. */
function disagreeing(scores: Scores): ScoredUnit[] {
  const units: ScoredUnit[] = ["content", "data"];
  return units.filter((u) => (scores[u] ?? 1) < SOFT);
}

/** What the rebuild check adds to an exact match, whose name already says the Instance-Code is equal. */
function exactDetail(found: Found): string {
  const r = found.rebuild?.status;
  if (r === "passed") return "Rebuild check passed";
  if (r === "failed") return "Rebuild check failed";
  if (r === "partial") return "Hash covers part of the file";
  return "Instance-Code equal";
}

/** The kind of match of a found manifest (`tiers.ts`), the rule that gives it, the scores or checks it rests on, and
 * the state that colours it. */
interface Kind {
  tier: Tier | null;
  name: string;
  rule: string;
  detail: string;
  state: string;
}

/** The state of each kind of match, for the dot of the summary row; a kind left out is weak (grey). */
const KIND_STATE: Partial<Record<Tier, string>> = {
  exact: "instance",
  near: "similar",
  reencoded: "similar",
  adaptation: "related",
  corroborated: "related",
  incoherent: "failed",
};

/** How a found manifest matches this file: its kind from the unit scores, with what C2PA and the rebuild check add. */
function matchKind(found: Found, scores: Scores): Kind {
  const t = tier(scores);
  const { name, rule } = t ? TIERS[t] : UNRANKED;
  const valid = found.for_file.status === "valid";
  const rebuild = found.rebuild?.status;
  let detail = evidence(scores);
  if (valid) detail = "Hash matches this file";
  else if (t === "exact") detail = exactDetail(found);
  else if (t === "incoherent") detail = `Instance-Code equal · ${unitList(scores, disagreeing(scores))}`;
  let state = (t && KIND_STATE[t]) || "weak";
  if (valid || (t === "exact" && rebuild === "passed")) state = "exact";
  else if (rebuild === "failed") state = "failed";
  return { tier: t, name, rule, detail, state };
}

/** The summary row of a found manifest: claim thumbnail, signer, title and verified day, and its kind of match with
 * the scores it rests on; it expands the card. The signer comes first, and the title in quotes is the signer's. */
function summaryRow(found: Found, open: boolean, inspection: Inspection, kind: Kind): string {
  const m = found.manifest;
  const thumb = m.thumbnail ? `<img src="${esc(m.thumbnail)}" alt="" draggable="false" />` : kindIcon(inspection.kind);
  const time = verifiedTime(m);
  const when = time ? `<span title="${esc(time)}">${esc(minute(time))}, verified</span>` : "no verified time";
  const title = m.title ? `“${esc(m.title)}” · ` : "";
  const tag = m.test_key ? `<span class="tag" title="${esc(TEST_KEY)}">public test key</span>` : "";
  return `
    <button type="button" class="match-head" data-expand="${esc(found.store_id)}" aria-expanded="${open}">
      <span class="thumb">${thumb}</span>
      <span class="what">
        <span class="signer"><span class="name">${esc(signerName(m))}</span>${tag}</span>
        <span class="sub">${title}${when}</span>
      </span>
      <span class="label" data-state="${kind.state}" title="${esc(`${kind.name}: ${kind.rule}`)}">
        <span class="kind"><span class="dot"></span>${esc(kind.name)}</span>
        <span class="evidence">${esc(kind.detail)}</span>
      </span>
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

/** Why recovered credentials are rejected when their hash does not match. */
const MISMATCH_EXPECTED =
  "For recovered credentials this is expected: the hash alone cannot tell whether this file was changed or the credentials belong to other content.";

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

/** The ISCC verdict: the kind of match and the rule that gives it, then what an equal Instance-Code proves, or how
 * the Content-Code compares with `near`, the lowest near-identical similarity; never in the signer's name. */
function isccVerdict(found: Found, near: number, kind: Kind): string {
  const title = `${kind.name}: ${kind.rule}`;
  const exact =
    "This file has exactly the bytes the ISCC soft binding of these credentials was computed from (a BLAKE3 hash of 256 bits). That alone ties none of their other statements to this file; only C2PA validation, above, can.";
  if (kind.tier === "incoherent") {
    return verdictBlock(
      "failed",
      title,
      esc(`Their other units do not fit that Instance-Code, so the soft binding does not describe one file. ${exact}`),
    );
  }
  if (found.iscc.exact) return verdictBlock("exact", title, esc(exact));
  const content = contentText(found);
  if (found.iscc.near) {
    return verdictBlock(
      "similar",
      title,
      esc(
        `With ${content}, their ISCC soft binding was computed from another file with near-identical content, perhaps the one this file was re-encoded or resized from. What differs is not covered by these credentials.`,
      ),
    );
  }
  const detail = content
    ? `${content}, below the ${percent(near)} of a near-identical match. Their ISCC soft binding was computed from another file; how the two relate is not recorded.`
    : "Their ISCC soft binding was computed from another file and lists no Content-Code this app can compare; how the two relate is not recorded.";
  return verdictBlock("related", title, esc(detail));
}

/** The first part of each unit's name in the comparison strip; "-Code" follows it while the card is wide enough. */
const CELL_NAME: Record<string, string> = { meta: "Meta", semantic: "Semantic", content: "Content", data: "Data", instance: "Instance" };

/** What a tile shows for `mt`: the value, the share of its meter (0 to 1) and a note. A Semantic-Code is compared only
 * while this file's is shown; the Instance-Code is equal or not. */
function cellValue(mt: UnitMatch, semantic: boolean): [string, number, string] {
  const unit = mt.embedded.unit;
  if (unit === "semantic" && !semantic) return ["—", 0, "not compared"];
  if (mt.similarity === null) return ["—", 0, "not comparable"];
  if (unit === "instance") return mt.similarity === 1 ? ["Match", 1, "the same bytes"] : ["No match", 0, "other bytes"];
  return [percent(mt.similarity), mt.similarity, unit === "semantic" ? "experimental" : ""];
}

/** One unit of a soft binding against this file's, as a tile: name, value, a meter in the unit's colour, grey below
 * 60%, where a unit counts for no kind of match, and a note. */
function unitCell(mt: UnitMatch, semantic: boolean): string {
  const unit = mt.embedded.unit;
  const [value, share, note] = cellValue(mt, semantic);
  const weak = unit !== "instance" && share < WEAK ? " weak" : "";
  return `
            <div class="tile">
              <div class="tile-name" title="${esc(mt.embedded.iscc)}"><span class="swatch" data-unit="${unit}"></span><span>${CELL_NAME[unit] ?? esc(mt.embedded.name)}<span class="suffix">-Code</span></span></div>
              <div class="tile-value">${value}</div>
              <div class="tile-bar${weak}" data-unit="${unit}"><i style="width:${Math.round(share * 100)}%"></i></div>
              <div class="tile-note">${note || "&nbsp;"}</div>
            </div>`;
}

/** One soft binding's units against this file's, as a strip of tiles. */
function unitStrip(sb: SoftBindingSummary, semantic: boolean): string {
  const scope = sb.scoped ? `<p class="hint strip-scope">For part of the file</p>` : "";
  return `${scope}<div class="strip">${sb.matches.map((mt) => unitCell(mt, semantic)).join("")}</div>`;
}

/** Each ISCC soft binding's units against this file's, as strips of tiles. */
function unitStrips(found: Found, semantic: boolean): string {
  return found.manifest.soft_bindings
    .filter((sb) => sb.supported && !sb.error)
    .map((sb) => unitStrip(sb, semantic))
    .join("");
}

/** This app's ISCC check (IEP-0020): the verdict for the units compared at the top of the card, and what keeps the
 * match from being complete or attributed to the signer. */
function isccBox(found: Found, near: number, kind: Kind): string {
  const notes = [
    "The tiles at the top compare their units with this file's as the left column shows them; a grey bar is below 60%, where a unit counts for no kind of match.",
  ];
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
    `${isccVerdict(found, near, kind)}<div class="note">${esc(notes.join(" "))}</div>`,
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

/** The expanded card of a found manifest: its units against this file's first, then C2PA, the picture, the ISCC
 * check and the details. */
function details(found: Found, { inspection, semantic, near }: Against, kind: Kind): string {
  const m = found.manifest;
  return `
    <div class="match-body">
      ${unitStrips(found, semantic)}
      ${part(head("C2PA"), `${c2paVerdict(found, inspection.mime)}${restoreRow(found)}`)}
      ${compareBox(m, inspection)}
      ${isccBox(found, near, kind)}
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
  const kind = matchKind(found, unitScores(found, against.semantic));
  return `
    <section class="card match" data-open="${open}">
      ${summaryRow(found, open, against.inspection, kind)}
      ${open ? details(found, against, kind) : ""}
    </section>`;
}

/** How the list is made, behind the help button of its heading; `near` is the lowest near-identical similarity. */
function order(near: number): string {
  return `Found by this file's ISCC on the ISCC test network. Each manifest store was fetched and checked here; the services' own scores only decide which are fetched. First come near-identical matches (Content-Code at ${percent(near)} or more), those with an equal Instance-Code first, then by earliest verified signing time; then files with related content, the strongest kind of match first. Each names its kind of match from the units that agree, by the tiers of iscc-hub's ranking: high at 85%, agreeing at 70%, backing up at 60%.`;
}

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
  return `<p class="found-warning">${esc(`Recovered credentials are not valid for this file as it is${unless}, and their content must not be attributed to their signers for this file. Open one for C2PA's verdict and this app's checks.`)}</p>`;
}

/** `list` sorted by kind of match, the strongest first, keeping its order within a kind. */
function byKind(list: Found[], semantic: boolean): Found[] {
  const rank = new Map(list.map((f) => [f, tierRank(tier(unitScores(f, semantic)))]));
  return [...list].sort((a, b) => (rank.get(a) ?? 0) - (rank.get(b) ?? 0));
}

/** What a search found, as cards (those in `open` expanded): the near-identical matches, then those with related
 * content by kind of match, and what was left out. */
export function foundList(lookup: Lookup, open: string[], inspection: Inspection, semantic: boolean): string {
  const against = { inspection, semantic, near: lookup.near_identical };
  const card = (f: Found) => matchCard(f, open.includes(f.store_id), against);
  const listed = [...lookup.matches, ...byKind(lookup.related, semantic)];
  const n = listed.length;
  const heading =
    n > 0
      ? `<div class="found-head"><h3>${n === 1 ? "1 match" : `${n} matches`} on the ISCC test network</h3>${helpTip(order(lookup.near_identical))}</div>`
      : "";
  return `<div class="found-list">${heading}${warning(listed)}${listed.map(card).join("")}${notes(lookup)}</div>`;
}
