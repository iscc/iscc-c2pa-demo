// Content Credentials tab: validation status, ISCC soft binding, CAWG training-mining and assertions.

import type {
  AssertionSummary,
  BindingMetadata,
  Inspection,
  ManifestSummary,
  TimestampSummary,
  TrainingEntry,
  UnitMatch,
  ValidationStatus,
} from "../api";
import { contentSource, stripCaveat } from "../formats";
import { esc, formatTime, fullStop, isccHtml, json, percent, shortUri } from "../util";

const STATE_TEXT: Record<string, string> = {
  Trusted: "Valid, signer on a trust list",
  Valid: "Valid, signer not on a trust list",
  Invalid: "Invalid",
};

/** Name and explanation of each bundled signer trust list, keyed by its `trust_uri` in `context.rs`. */
const TRUST_LISTS: Record<string, { name: string; about: string }> = {
  "https://github.com/c2pa-org/conformance-public/blob/main/trust-list/C2PA-TRUST-LIST.pem": {
    name: "the C2PA trust list",
    about: "Official C2PA trust list of conforming products, bundled with this app.",
  },
  "urn:c2pa-rs:test-root-bundle": {
    name: "the c2pa-rs test list",
    about:
      "Test certificates from c2pa-rs, including this demo's signer. Only this app trusts them; other validators report the signer as unknown.",
  },
};

/** Validation state label; a trusted signer names the trust list its certificate chains to. */
function stateText(m: ManifestSummary): string {
  const list = TRUST_LISTS[m.validation?.trustListUri ?? ""];
  if (m.validation_state === "Trusted" && list) return `Valid, signer on ${list.name}`;
  return STATE_TEXT[m.validation_state] ?? m.validation_state;
}

/** Hover text of the validation state: what the trust list is, or its URI when unknown. */
function stateTitle(m: ManifestSummary): string {
  if (m.validation_state !== "Trusted") return "";
  const uri = m.validation?.trustListUri ?? "";
  return TRUST_LISTS[uri]?.about ?? uri;
}

/** Label of each timestamp state; the colour of its dot never carries the meaning alone. */
const TIMESTAMP_TEXT: Record<TimestampSummary["status"], string> = {
  trusted: "Verified time",
  untrusted: "Unverified time",
  none: "No timestamp",
  rejected: "Timestamp rejected",
};

/** Sentence under the timestamp state: what the timestamp proves, as HTML. */
function timestampSentence(ts: TimestampSummary): string {
  const time = ts.time ? `<span title="${esc(ts.time)}">${esc(formatTime(ts.time))}</span>` : "an unknown time";
  const tsa = `<span title="${esc(ts.tsa_detail ?? "")}">${esc(ts.tsa ?? "The timestamp service")}</span>`;
  switch (ts.status) {
    case "trusted":
      return ts.legacy
        ? `Signed on or before ${time}. Confirmed by ${tsa}${fullStop(ts.tsa ?? "")} This older (v1) manifest format does not check the timestamp service against the C2PA trust list.`
        : `Signed on or before ${time}. Confirmed by ${tsa}, a timestamp service on the C2PA trust list.`;
    case "untrusted":
      return `${tsa} vouches for ${time}, but it is not on the C2PA trust list, so validators do not rely on it.`;
    case "rejected":
      return `The timestamp is invalid and is ignored (${esc(ts.reason ?? "no reason given")}). The signing time is not proven.`;
    default:
      return "The signing time is not proven. The signature can only be checked while the signer's certificate is valid.";
  }
}

/** Timestamp row of the status card. */
function timestampRow(ts: TimestampSummary | undefined): string {
  if (!ts) return "";
  return `<dt>Timestamp</dt><dd><span class="status" data-ts="${esc(ts.status)}"><span class="dot"></span>${esc(TIMESTAMP_TEXT[ts.status] ?? ts.status)}</span><span class="hint">${timestampSentence(ts)}</span></dd>`;
}

/** Well-known CAWG use cases with display names, in spec order. */
export const USE_CASES: { key: string; label: string }[] = [
  { key: "cawg.ai_training", label: "AI training" },
  { key: "cawg.ai_generative_training", label: "Generative AI training" },
  { key: "cawg.data_mining", label: "Data mining" },
  { key: "cawg.ai_inference", label: "AI inference" },
];

const USE_TEXT: Record<string, string> = {
  allowed: "Allowed",
  notAllowed: "Not allowed",
  constrained: "Constrained",
};

/** True when the manifest carries a soft binding with the ISCC algorithm. */
function hasIsccBinding(m: ManifestSummary): boolean {
  return m.soft_bindings.some((sb) => sb.supported);
}

/** True when the file could get an ISCC soft binding it does not have yet (readable store or none). */
export function needsIsccBinding(inspection: Inspection): boolean {
  return !inspection.manifest_error && !(inspection.manifest && hasIsccBinding(inspection.manifest));
}

/** Whole tab. */
export function credentialsTab(inspection: Inspection): string {
  if (inspection.manifest_error) {
    return `
      <section class="card"><div class="center">
        <h3>Content Credentials could not be read</h3>
        <p class="mono">${esc(inspection.manifest_error)}</p>
      </div></section>`;
  }
  const m = inspection.manifest;
  if (!m) {
    return `
      <section class="card"><div class="center">
        <h3>No Content Credentials</h3>
        <p>This file carries no C2PA manifest. Sign it to add one with an ISCC soft binding.</p>
        <button class="btn primary" data-goto="sign">Sign this file</button>
      </div></section>`;
  }
  return [
    statusCard(m),
    softBindingCard(m, inspection),
    trainingCard(m),
    actionsCard(m),
    ingredientsCard(m),
    validationCard(m),
    assertionsCard(m.assertions),
  ]
    .filter(Boolean)
    .join("");
}

function statusCard(m: ManifestSummary): string {
  const s = m.signature;
  const signer = [s?.common_name, s?.issuer].filter(Boolean).join(" · ") || "unknown";
  return `
    <section class="card">
      <header>
        <span class="status" data-state="${esc(m.validation_state)}" title="${esc(stateTitle(m))}"><span class="dot"></span>${esc(stateText(m))}</span>
        <span class="grow"></span>
        <span class="hint">${m.manifest_count} manifest${m.manifest_count === 1 ? "" : "s"} in store</span>
      </header>
      <div class="body">
        <dl class="kv">
          <dt>Title</dt><dd>${esc(m.title ?? "—")}</dd>
          <dt>Claim generator</dt><dd>${esc(m.claim_generator ?? "—")}</dd>
          <dt>Signer</dt><dd>${esc(signer)}</dd>
          <dt>Signature</dt><dd>${esc(s?.alg?.toUpperCase() ?? "—")}</dd>
          ${timestampRow(s?.timestamp)}
          <dt>Manifest label</dt><dd class="mono">${esc(m.label)}</dd>
        </dl>
      </div>
    </section>`;
}

function softBindingCard(m: ManifestSummary, inspection: Inspection): string {
  const missing = hasIsccBinding(m) ? "" : missingIsccCard();
  return (
    missing +
    m.soft_bindings
      .map((sb) => {
        if (!sb.supported || sb.error) {
          return `
          <section class="card">
            <header><h2>Soft binding</h2><span class="grow"></span><span class="hint mono">${esc(sb.alg ?? "no algorithm")}</span></header>
            <div class="note">${esc(sb.error ?? "This algorithm is not decoded by the demo.")}</div>
            ${bindingMetadata(sb.metadata)}
          </section>`;
        }
        const rows = sb.matches
          .map((mt) => {
            const u = mt.embedded;
            return `
            <tr>
              <td class="unit-name"><span class="swatch" data-unit="${u.unit}"></span>${esc(u.name)}</td>
              <td><span class="mono">${isccHtml(u.iscc)}</span></td>
              <td class="num">${verdict(mt)}</td>
            </tr>`;
          })
          .join("");
        return `
        <section class="card">
          <header><h2>Soft binding</h2><span class="grow"></span><span class="hint mono">${esc(sb.alg)}</span></header>
          <table>
            <thead><tr><th>Unit</th><th>Embedded in manifest</th><th style="text-align:right">Match with this file</th></tr></thead>
            <tbody>${rows}</tbody>
          </table>
          <div class="note">${esc(matchNote(sb.matches, inspection))}</div>
          ${bindingMetadata(sb.metadata)}
        </section>`;
      })
      .join("")
  );
}

/** Match column: yes or no for the exact Instance-Code, a similarity meter for the other units. */
function verdict(mt: UnitMatch): string {
  if (mt.similarity === null) return `<span class="hint">not compared</span>`;
  if (mt.embedded.unit === "instance") {
    const exact = mt.similarity === 1;
    return `<span class="exact" data-match="${exact}"><span class="dot"></span>${exact ? "Match" : "No match"}</span>`;
  }
  const pct = Math.round(mt.similarity * 100);
  return `<span class="similarity"><span class="meter"><i style="width:${pct}%"></i></span>${percent(mt.similarity)}</span>`;
}

/** Explains how each unit in the match column was recomputed, for the units present. */
function matchNote(matches: UnitMatch[], inspection: Inspection): string {
  const has = (unit: string) => matches.some((mt) => mt.embedded.unit === unit);
  const parts: string[] = [];
  if (has("content")) parts.push(`Content-Code is recomputed from this file's ${contentSource(inspection)}.`);
  if (has("data") || has("instance")) {
    parts.push(
      "Data-Code and Instance-Code are recomputed from this file without its manifest store: Data-Code tolerates small byte changes, Instance-Code is exact and either matches or not.",
    );
    const caveat = stripCaveat(inspection);
    if (caveat) parts.push(caveat.note);
  }
  if (has("meta")) {
    parts.push(
      "Meta-Code is recomputed from this file's title and description: the file's own metadata first, then the title and description stored in the Content Credentials at signing, then the manifest title, then the file name. Changing an embedded title at signing therefore lowers the match.",
    );
  }
  return parts.join(" ");
}

/** Call to action for a manifest without an ISCC soft binding, the gap this demo exists to close. */
function missingIsccCard(): string {
  return `
    <section class="card callout">
      <header><h2>No ISCC soft binding</h2><span class="grow"></span><span class="hint mono">c2pa.soft-binding</span></header>
      <div class="body">
        <p>
          These Content Credentials are tied to the file by a cryptographic hash only. Once the manifest is
          stripped or the file is re-encoded, that link is gone. An ISCC soft binding embeds the ISCC of the
          content in the manifest, so a stripped or re-encoded copy can still be matched back to it.
        </p>
        <div class="cta">
          <button class="btn primary" data-goto="sign">Add ISCC soft binding</button>
          <span class="hint">Signs a copy and keeps this manifest as its parent ingredient.</span>
        </div>
      </div>
    </section>`;
}

/** Description, contact and link from the assertion's `bindingMetadata`; empty when absent. */
function bindingMetadata(md: BindingMetadata | null): string {
  if (!md) return "";
  const rows: string[] = [];
  if (md.description) rows.push(`<dt>About</dt><dd>${esc(md.description)}</dd>`);
  if (md.contact) rows.push(`<dt>Contact</dt><dd class="mono">${esc(md.contact)}</dd>`);
  if (md.informational_url)
    rows.push(`<dt>Details</dt><dd><a class="ext" href="${esc(md.informational_url)}">${esc(linkText(md.informational_url))}</a></dd>`);
  return `<dl class="kv meta">${rows.join("")}</dl>`;
}

/** URL without scheme and trailing slash, for display. */
function linkText(url: string): string {
  return url.replace(/^https?:\/\//, "").replace(/\/$/, "");
}

function trainingCard(m: ManifestSummary): string {
  const entries = m.training_mining?.entries;
  if (!entries) {
    return `
      <section class="card">
        <header><h2>Training and data mining</h2><span class="grow"></span><span class="hint mono">cawg.training-mining</span></header>
        <div class="note">No CAWG training and data mining assertion.</div>
      </section>`;
  }
  const known = USE_CASES.filter((u) => u.key in entries).map((u) => useRow(u.label, u.key, entries[u.key]));
  const custom = Object.keys(entries)
    .filter((k) => !USE_CASES.some((u) => u.key === k))
    .map((k) => useRow(k, k, entries[k]));
  return `
    <section class="card">
      <header><h2>Training and data mining</h2><span class="grow"></span><span class="hint mono">cawg.training-mining</span></header>
      <table><tbody>${[...known, ...custom].join("")}</tbody></table>
    </section>`;
}

function useRow(label: string, key: string, entry: TrainingEntry | null | undefined): string {
  const name = `<td><div>${esc(label)}</div><div class="mono" style="color:var(--muted)">${esc(key)}</div></td>`;
  // Signature validation does not check the assertion's shape; show malformed entries instead of failing.
  if (!entry || typeof entry !== "object") {
    return `<tr>${name}<td><span class="chip">Malformed</span><div class="hint mono">${esc(json(entry ?? null))}</div></td></tr>`;
  }
  const info = entry.constraint_info ? `<div class="hint">${esc(entry.constraint_info)}</div>` : "";
  return `
    <tr>
      ${name}
      <td><span class="chip" data-use="${esc(entry.use)}">${esc(USE_TEXT[entry.use] ?? entry.use)}</span>${info}</td>
    </tr>`;
}

function actionsCard(m: ManifestSummary): string {
  const assertion = m.assertions.find((a) => a.label.startsWith("c2pa.actions"));
  const actions = (assertion?.data as { actions?: Record<string, unknown>[] } | undefined)?.actions ?? [];
  if (actions.length === 0) return "";
  const rows = actions
    .map((a) => {
      const agent = a.softwareAgent as { name?: string; version?: string } | string | undefined;
      const agentText = typeof agent === "string" ? agent : agent ? [agent.name, agent.version].filter(Boolean).join(" ") : "";
      return `
        <tr>
          <td class="mono">${esc(a.action)}</td>
          <td class="mono" style="color:var(--muted)">${esc(shortUri(a.digitalSourceType as string | undefined))}</td>
          <td>${esc(agentText || (a.when as string | undefined) || "")}</td>
        </tr>`;
    })
    .join("");
  return `
    <section class="card">
      <header><h2>Actions</h2><span class="grow"></span><span class="hint mono">${esc(assertion?.label ?? "")}</span></header>
      <table><thead><tr><th>Action</th><th>Digital source type</th><th>Agent</th></tr></thead><tbody>${rows}</tbody></table>
    </section>`;
}

function ingredientsCard(m: ManifestSummary): string {
  if (m.ingredients.length === 0) return "";
  const rows = m.ingredients
    .map(
      (i) => `
      <tr>
        <td>${esc(i.title ?? "untitled")}</td>
        <td class="mono">${esc(i.relationship)}</td>
        <td class="mono" style="color:var(--muted)">${esc(i.format ?? "")}</td>
        <td>${i.validation_state ? `<span class="status" data-state="${esc(i.validation_state)}"><span class="dot"></span>${esc(i.validation_state)}</span>` : ""}</td>
      </tr>`,
    )
    .join("");
  return `
    <section class="card">
      <header><h2>Ingredients</h2></header>
      <table><thead><tr><th>Title</th><th>Relationship</th><th>Format</th><th>Validation</th></tr></thead><tbody>${rows}</tbody></table>
    </section>`;
}

function validationCard(m: ManifestSummary): string {
  const v = m.validation?.activeManifest;
  const groups: [string, ValidationStatus[], boolean][] = [
    ["Failures", v?.failure ?? [], true],
    ["Informational", v?.informational ?? [], false],
    ["Passed checks", v?.success ?? [], false],
  ];
  const sections = groups
    .filter(([, list]) => list.length > 0)
    .map(
      ([name, list, open]) => `
      <details ${open ? "open" : ""}>
        <summary>${esc(name)} <span class="count">${list.length}</span></summary>
        <ul class="validation-list">
          ${list.map((s) => `<li><span class="mono">${esc(s.code)}</span><span>${esc(s.explanation ?? "")}</span></li>`).join("")}
        </ul>
      </details>`,
    )
    .join("");
  return `
    <section class="card">
      <header><h2>Validation</h2><span class="grow"></span><span class="hint">${esc(m.validation?.specVersion ? `spec ${m.validation.specVersion}` : "")}</span></header>
      ${sections || `<div class="note">No validation details available.</div>`}
    </section>`;
}

function assertionsCard(assertions: AssertionSummary[]): string {
  if (assertions.length === 0) return "";
  return `
    <section class="card">
      <header><h2>All assertions</h2><span class="grow"></span><span class="hint">${assertions.length}</span></header>
      ${assertions
        .map(
          (a) => `
        <details>
          <summary><span class="mono">${esc(a.label)}</span></summary>
          <pre class="json">${esc(json(a.data))}</pre>
        </details>`,
        )
        .join("")}
    </section>`;
}
