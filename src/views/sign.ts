// Sign tab: manifest details, ISCC unit selection, CAWG training-mining choices, credentials and
// the timestamp service.

import type { AppInfo, Credentials, Inspection, IsccUnit, SignRequest, SignResult, TrainingEntry, TsaPreset } from "../api";
import { isccCode } from "../composite";
import { contentSource, embedding } from "../formats";
import { chosenCode, newSourceChoice, type SourceChoice, termUri } from "../sourcetype";
import { esc, fullStop, isccHtml, splitPath } from "../util";
import { USE_CASES } from "./credentials";
import { helpTip } from "./icons";
import { sourceFieldset } from "./source";

const SIGNING_ALGS = ["es256", "es384", "es512", "ps256", "ps384", "ps512", "ed25519"];

interface UnitOption {
  slug: string;
  unit: IsccUnit["unit"];
  title: string;
  desc: string;
}

/** The Semantic-Code offered for an image or a text; none for audio and video. */
function semanticOption(inspection: Inspection): UnitOption[] {
  if (inspection.kind === "image") {
    return [
      {
        slug: "semantic",
        unit: "semantic",
        title: "Semantic-Code Image",
        desc: "Experimental. Sign bits of a neural image descriptor (ISC21). Survives crops, recolouring, overlays and re-composition that the Content-Code Image does not. Codes may differ from iscc-sci by a few bits, because the model is compressed.",
      },
    ];
  }
  if (inspection.kind === "text") {
    return [
      {
        slug: "semantic",
        unit: "semantic",
        title: "Semantic-Code Text",
        desc: "Experimental. Sign bits of a multilingual sentence embedding averaged over the whole text. Matches translations, paraphrases and heavy edits that the Content-Code Text does not. Codes may differ from iscc-sct by a few bits, because the model is compressed.",
      },
    ];
  }
  return [];
}

/** What a signing writes: a signed copy whose manifest is then registered on the ISCC test network, a signed copy, or
 * a sidecar next to the file, which stays as it is. */
export type SignMode = "register" | "embed" | "sidecar";

/** What registering makes public, said beside every action that registers. */
export const REGISTER_NOTE =
  "Registering uploads the manifest store to the public ISCC test network: signer, title, description, ISCC and thumbnail. It stays there. The file itself is never uploaded.";

/** What the ISCC soft binding section adds to the manifest. */
const BINDING_HELP =
  "A c2pa.soft-binding assertion with the algorithm io.iscc.v0 (IEP-0020): the ticked ISCC-UNITs of this file, which find its manifest again from any copy.";

/** What the training and data mining section adds to the manifest. */
const TRAINING_HELP =
  "A cawg.training-mining assertion with cawg.ai_training, cawg.ai_generative_training, cawg.data_mining and cawg.ai_inference. Leave all unset to omit it.";

/** What the Sign tab needs to know about the app besides the file. */
export interface SignContext {
  /** True while the background pass computes units of this file: signing waits for it. */
  analysing: boolean;
  /** True while the menu of the signing actions is open. */
  menu: boolean;
  /** Whether the file's Semantic-Code is offered: its kind is on and the inspection mentions it. */
  semantic: boolean;
}

/** The Content-Code offered for an asset: image, text, audio or video. */
function contentOption(inspection: Inspection): UnitOption {
  if (inspection.kind === "text") {
    return {
      slug: "text",
      unit: "content",
      title: "Content-Code Text",
      desc: "Similarity hash of the text in reading order. Survives reformatting, re-packaging and metadata edits.",
    };
  }
  if (inspection.kind === "audio") {
    return {
      slug: "audio",
      unit: "content",
      title: "Content-Code Audio",
      desc: `Chromaprint fingerprint of the ${contentSource(inspection)}. Survives re-encoding, format changes and metadata edits.`,
    };
  }
  if (inspection.kind === "video") {
    return {
      slug: "video",
      unit: "content",
      title: "Content-Code Video",
      desc: "MPEG-7 signatures of the video frames, five per second. Survives re-encoding, resizing and container changes.",
    };
  }
  return {
    slug: "image",
    unit: "content",
    title: "Content-Code Image",
    desc: `Perceptual hash of the ${contentSource(inspection)}. Survives re-encoding, resizing and metadata stripping.`,
  };
}

/** What the Instance-Code will prove after signing (IEP-0020, Source Preservation). */
function afterSigning(inspection: Inspection): string {
  if (inspection.manifest) {
    return "This file already has Content Credentials, which signing replaces, so after signing Data-Code and Instance-Code identify this file, not the signed copy.";
  }
  const e = embedding(inspection);
  if (e.kind === "preserved") {
    return "Signing only inserts the Content Credentials, so it will show that the signed copy without them is this file, byte for byte.";
  }
  return `After signing, Data-Code and Instance-Code identify this file, not the signed copy: ${e.kind === "changed" ? "signing " : ""}${e.why}.`;
}

/** Units offered for an asset, all computed from the file exactly as it is (IEP-0020); the Semantic-Code only when
 * `semantic` says it is shown. */
function unitOptions(inspection: Inspection, semantic: boolean): UnitOption[] {
  return [
    {
      slug: "meta",
      unit: "meta",
      title: "Meta-Code",
      desc: "Similarity hash of title and description, both stored in the Content Credentials (cawg.metadata). Needs a title.",
    },
    ...(semantic ? semanticOption(inspection) : []),
    contentOption(inspection),
    {
      slug: "data",
      unit: "data",
      title: "Data-Code",
      desc: "Similarity hash of the file's bytes. Tolerates small byte-level edits, not re-encoding. Required by IEP-0020.",
    },
    {
      slug: "instance",
      unit: "instance",
      title: "Instance-Code",
      desc: `Cryptographic hash of the file's bytes, the identity of this exact file. Required by IEP-0020. ${afterSigning(inspection)}`,
    },
  ];
}

/** Value of the timestamp select that reveals the URL field. */
const CUSTOM_TSA = "custom";

/** Value of the timestamp select that signs without a timestamp. */
export const NO_TIMESTAMP = "off";

/** Timestamp choice: on or off, a preset URL or `CUSTOM_TSA`, and the custom URL. */
export interface TimestampChoice {
  on: boolean;
  preset: string;
  custom: string;
}

/** Mutable form state, kept across re-renders so a re-render never loses input. */
export interface SignForm {
  title: string;
  description: string;
  /** How the source was made, as answered or picked from the full list. */
  source: SourceChoice;
  units: Set<string>;
  /** Which sections that start closed are open: the ISCC soft binding shows its units, training and data mining its
   * uses one by one. */
  open: Record<Fold, boolean>;
  training: Record<string, TrainingEntry>;
  credentials: Credentials;
  timestamp: TimestampChoice;
  output: string;
  metaPreview: IsccUnit | null;
  /** Why the backend could not compute the Meta-Code preview for a non-empty title. */
  metaError: string | null;
}

/** The sections of the form that start closed. */
export type Fold = "binding" | "training";

/** The header of a section that starts closed: its title, the space after it and the chevron open and close it; the
 * info tooltip explains it, and `control` (the choice for all uses) sits before the chevron. */
function foldHeader(fold: Fold, open: boolean, title: string, help: string, control = ""): string {
  const hidden = `data-fold="${fold}" aria-expanded="${open}" tabindex="-1" aria-hidden="true"`;
  return `
        <div class="legend fold-head">
          <button type="button" class="toggle" data-fold="${fold}" aria-expanded="${open}">${title}</button>${helpTip(help)}
          <button type="button" class="toggle grow" ${hidden}></button>
          ${control}
          <button type="button" class="toggle chev-btn" ${hidden}><span class="chev"></span></button>
        </div>`;
}

/** A setting on one line: its label and tooltip on the left, its control on the right. */
function settingLine(id: string, label: string, help: string, control: string): string {
  return `<div class="legend setting"><label for="${id}">${label}</label>${helpTip(help)}${control}</div>`;
}

/** What signing does with a title and description that a PDF does not carry itself. */
const WRITES_METADATA =
  "Signing writes this title and description into the PDF copy, as a revision of its own, and computes every ISCC unit from that copy.";

/** Whether signing writes the form's title and description into the copy: a PDF that does not carry them itself. The
 * backend decides the same on sanitised values; white space is all that differs here. */
export function writesMetadata(form: SignForm, inspection: Inspection): boolean {
  if (inspection.mime !== "application/pdf") return false;
  const own = inspection.meta_fields;
  const clean = (s: string | null) => (s ?? "").trim().replace(/\s+/g, " ");
  return own.name_source !== "metadata" || clean(form.title) !== clean(own.name) || clean(form.description) !== clean(own.description);
}

/** Placeholder for the Meta-Code preview when there is no code to show. */
export function metaPreviewHint(form: SignForm): string {
  return form.metaError ? `not available: ${form.metaError}` : "enter a title to preview";
}

/** Fresh form state for a newly loaded file; timestamps with the first preset. The Semantic-Code of an image or a
 * text starts ticked when it is computed or on its way (its kind switched on). */
export function newSignForm(inspection: Inspection, presets: TsaPreset[]): SignForm {
  const semantic = semanticOption(inspection).length > 0 && semanticAvailable(inspection);
  return {
    title: inspection.meta_fields.name,
    description: inspection.meta_fields.description ?? "",
    source: newSourceChoice(),
    units: new Set(["meta", ...(semantic ? ["semantic"] : []), inspection.kind, ...REQUIRED_UNITS]),
    open: { binding: false, training: false },
    training: {},
    credentials: { kind: "demo" },
    timestamp: { on: presets.length > 0, preset: presets[0]?.url ?? CUSTOM_TSA, custom: "" },
    output: inspection.suggested_output,
    metaPreview: null,
    metaError: null,
  };
}

/** Whether the file's Semantic-Code is computed or on its way. */
function semanticAvailable(inspection: Inspection): boolean {
  return inspection.iscc.some((u) => u.unit === "semantic") || inspection.pending.includes("semantic");
}

/** Untick the units the finished analysis could not compute (a video without frames, a text the models could not
 * embed); every other choice stays as the user left it. */
export function dropFailedUnits(form: SignForm, inspection: Inspection) {
  if (inspection.content_error) form.units.delete(inspection.kind);
  if (inspection.semantic_error) form.units.delete("semantic");
}

/** Problems that keep the form from being signed in `mode`; empty when it is complete. */
export function signProblems(form: SignForm, inspection: Inspection, mode: SignMode): string[] {
  const problems: string[] = [];
  if (inspection.sign_block) problems.push(inspection.sign_block);
  if (!form.title.trim()) problems.push("A title is required.");
  if (form.units.has("meta") && !form.title.trim()) problems.push("The Meta-Code needs a title.");
  for (const [key, entry] of Object.entries(form.training)) {
    if (entry.use === "constrained" && !entry.constraint_info?.trim()) problems.push(`Describe the constraint for ${key}.`);
  }
  if (form.credentials.kind === "custom" && (!form.credentials.cert_path || !form.credentials.key_path))
    problems.push("Choose a certificate and a private key.");
  if (form.timestamp.on && form.timestamp.preset === CUSTOM_TSA && !/^https?:\/\/\S+$/i.test(form.timestamp.custom.trim()))
    problems.push("Enter the timestamp service URL, starting with http:// or https://.");
  const output = outputProblem(form, inspection, mode);
  if (output) problems.push(output);
  return problems;
}

/** Why signing in `mode` cannot write its output: no sidecar next to this file, or a signed copy with another
 * extension; null when it can. */
function outputProblem(form: SignForm, inspection: Inspection, mode: SignMode): string | null {
  if (mode === "sidecar") return inspection.sidecar_block;
  const sourceExt = splitPath(inspection.path).ext.toLowerCase();
  return splitPath(form.output).ext.toLowerCase() === sourceExt ? null : `The output must keep the ${sourceExt} extension.`;
}

/** The key of the row that sets every use at once. */
export const ALL_USES = "*";

/** Set the training-mining choice `use` for `key`, or for every use with `ALL_USES` (empty clears it); a constraint
 * text is kept only while the choice is "constrained". */
export function setTrainingUse(form: SignForm, key: string, use: string) {
  if (key === ALL_USES) {
    for (const u of USE_CASES) setTrainingUse(form, u.key, use);
    return;
  }
  if (!use) delete form.training[key];
  else form.training[key] = { use, constraint_info: use === "constrained" ? (form.training[key]?.constraint_info ?? "") : undefined };
  if (use !== "constrained" && form.training[key]) delete form.training[key].constraint_info;
}

/** URL of the chosen timestamp service; undefined when timestamps are off. */
function tsaUrl(choice: TimestampChoice): string | undefined {
  if (!choice.on) return undefined;
  return choice.preset === CUSTOM_TSA ? choice.custom.trim() : choice.preset;
}

/** Host of a URL for messages, else the URL itself. */
function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

/** Success message after signing, and a note when the timestamp service failed. */
export function signedMessages(result: SignResult, mode: SignMode): { text: string; note: string | null } {
  const n = result.units.length;
  const units = `${n} ISCC unit${n === 1 ? "" : "s"}`;
  let text =
    mode === "sidecar"
      ? `Signed ${units} into ${splitPath(result.output).stem}.c2pa next to ${result.inspection.file_name}, which stays as it is.`
      : `Signed and embedded ${units} into ${result.inspection.file_name}.`;
  const ts = result.inspection.manifest?.signature?.timestamp;
  if (result.timestamp.status === "added" && ts) {
    const by = ts.tsa ?? "the timestamp service";
    text +=
      ts.status === "trusted"
        ? ` Timestamp verified by ${by}${fullStop(by)}`
        : ` Timestamped by ${by}, which is not on the C2PA trust list.`;
  }
  const note =
    result.timestamp.status === "failed"
      ? `Signed without a timestamp: ${hostOf(result.timestamp.url)} failed (${result.timestamp.reason}). The signature is valid; only the signing time is not proven. Sign again later to add one.`
      : null;
  return { text, note };
}

/** The digital source type URI the form records on the parent ingredient; none for a source with Content
 * Credentials, whose ingredient refers to its manifest instead. */
function sourceUri(form: SignForm, inspection: Inspection): string | undefined {
  const code = inspection.manifest ? null : chosenCode(inspection.kind, form.source);
  return code ? termUri(code) : undefined;
}

/** Build the request the Rust side expects; without the Semantic-Code unless it is shown (`semantic`), whatever the
 * form still holds. */
export function toRequest(form: SignForm, inspection: Inspection, semantic: boolean, mode: SignMode): SignRequest {
  return {
    source: inspection.path,
    output: form.output,
    title: form.title.trim(),
    description: form.description.trim() || undefined,
    meta: inspection.meta_fields.meta ?? undefined,
    source_type: sourceUri(form, inspection),
    units: [...form.units].filter((u) => semantic || u !== "semantic"),
    training: form.training,
    credentials: form.credentials,
    tsa_url: tsaUrl(form.timestamp),
    sidecar: mode === "sidecar",
  };
}

/** Timestamp fieldset: one select of no timestamp, the preset services and a custom URL, whose field shows under it. */
function timestampFieldset(choice: TimestampChoice, info: AppInfo | null): string {
  const selected = choice.on ? choice.preset : NO_TIMESTAMP;
  const option = (value: string, label: string) =>
    `<option value="${esc(value)}" ${value === selected ? "selected" : ""}>${esc(label)}</option>`;
  const presets = (info?.tsa_presets ?? []).map((p) => option(p.url, `${p.name} — ${p.trusted ? "on" : "not on"} the C2PA trust list`));
  const select = `<select id="f-tsa" name="ts_preset">${option(NO_TIMESTAMP, "No timestamp")}${presets.join("")}${option(CUSTOM_TSA, "Custom URL…")}</select>`;
  const help = `An RFC 3161 timestamp proves when the file was signed. Only a hash of the signature is sent. If the service does not answer within ${info?.tsa_timeout_secs ?? 10} s, the file is signed without a timestamp.`;
  const url =
    selected === CUSTOM_TSA
      ? `<div class="field">
          <label for="f-tsa-url">Service URL</label>
          <input type="text" class="mono" id="f-tsa-url" name="ts_custom" value="${esc(choice.custom)}" placeholder="https://" />
        </div>`
      : "";
  return `
      <div class="fieldset">
        ${settingLine("f-tsa", "Timestamp", help, select)}
        ${url}
      </div>`;
}

/** The code line under a unit option: the code, "computing…" while a later pass computes it, why the Semantic-Code
 * is not available, or the Meta-Code preview hint. */
function optionCode(o: UnitOption, form: SignForm, inspection: Inspection): string {
  const muted = (html: string, meta = false) =>
    `<span class="code"${meta ? ` data-meta-code="1"` : ""} style="color:var(--muted)">${html}</span>`;
  if (o.slug === "meta") {
    const code = form.metaPreview?.iscc;
    return code ? `<span class="code" data-meta-code="1">${isccHtml(code)}</span>` : muted(esc(metaPreviewHint(form)), true);
  }
  const unit = inspection.iscc.find((u) => u.name === o.title);
  if (unit) return `<span class="code">${isccHtml(unit.iscc)}</span>`;
  if (inspection.pending.includes(o.unit)) return muted("computing…");
  if (o.slug !== "semantic" || !inspection.semantic_error) return "";
  return muted(`not available: ${esc(inspection.semantic_error)}`);
}

/** Units every ISCC soft binding of the app carries: IEP-0020 requires the Data-Code and the Instance-Code of the
 * source in a block for the whole asset. */
const REQUIRED_UNITS = ["data", "instance"];

/** One unit option as a checkbox; a required unit is ticked and locked, and a hidden input sends it. */
function unitCheck(o: UnitOption, form: SignForm, inspection: Inspection): string {
  const required = REQUIRED_UNITS.includes(o.slug);
  const unavailable = o.slug === "semantic" && !semanticAvailable(inspection);
  const checked = required || (form.units.has(o.slug) && !unavailable);
  const name = required ? "" : `name="unit" value="${o.slug}"`;
  const sent = required ? `<input type="hidden" name="unit" value="${o.slug}" />` : "";
  return `
      <label class="check"${required ? ` title="Required by IEP-0020"` : ""}>
        <input type="checkbox" ${name} ${checked ? "checked" : ""} ${required || unavailable ? "disabled" : ""} />${sent}
        <span class="title"><span class="swatch" data-unit="${o.unit}"></span>${esc(o.title)}${helpTip(o.desc)}</span>
        ${optionCode(o, form, inspection)}
      </label>`;
}

/** The code a ticked unit option signs: the Meta-Code preview, else the unit the inspection computed; null while it
 * has none yet. */
function optionIscc(o: UnitOption, form: SignForm, inspection: Inspection): string | null {
  if (o.slug === "meta") return form.metaPreview?.iscc ?? null;
  return inspection.iscc.find((u) => u.name === o.title)?.iscc ?? null;
}

/** The ISCC-CODE line of the soft binding section: one stripe per ticked unit and the code they make, or what it
 * waits for. */
export function bindingSummary(form: SignForm, inspection: Inspection, semantic: boolean): string {
  const ticked = unitOptions(inspection, semantic).filter((o) => form.units.has(o.slug));
  const codes = ticked.map((o) => optionIscc(o, form, inspection));
  const missing = ticked.find((_, i) => codes[i] === null);
  const code = missing ? null : isccCode(codes as string[]);
  const stripes = ticked.map((o) => `<span data-unit="${o.unit}"></span>`).join("");
  const why = missing?.slug === "meta" ? `Meta-Code ${metaPreviewHint(form)}` : "waits for the units still being computed";
  const line = code ? `<div class="code">${isccHtml(code)}</div>` : `<div class="code" style="color:var(--muted)">${esc(why)}</div>`;
  return `
        <div class="unit" data-binding-summary>
          <span class="bar stripes">${code ? stripes : ""}</span>
          <div>
            <div class="name">ISCC-CODE of the ${ticked.length} ticked units</div>
            ${line}
          </div>
        </div>`;
}

/** The ISCC soft binding section: its header opens and closes it, the ISCC-CODE of the ticked units shows either way,
 * the unit options only while it is open (they stay in the form while closed, so their values are kept). */
function bindingFieldset(form: SignForm, inspection: Inspection, semantic: boolean): string {
  const checks = unitOptions(inspection, semantic)
    .map((o) => unitCheck(o, form, inspection))
    .join("");
  return `
      <div class="fieldset fold binding">
        ${foldHeader("binding", form.open.binding, "ISCC soft binding", BINDING_HELP)}
        ${bindingSummary(form, inspection, semantic)}
        <div class="units-choice" ${form.open.binding ? "" : "hidden"}>${checks}</div>
      </div>`;
}

const USE_CHOICES = [
  ["", "Unset"],
  ["allowed", "Allowed"],
  ["notAllowed", "Not allowed"],
  ["constrained", "Constrained"],
];

/** The choices of one row as a segmented control; `pressed` says per choice whether it holds for the row: "true",
 * "false", or "mixed" in the row for all uses when it holds for some of them. */
function useSegments(key: string, pressed: (use: string) => string): string {
  const buttons = USE_CHOICES.map(
    ([v, l]) => `<button type="button" data-use-key="${key}" data-use="${v}" aria-pressed="${pressed(v)}">${l}</button>`,
  ).join("");
  return `<div class="segmented">${buttons}</div>`;
}

/** One use with its choice, and the details a constraint needs. */
function useRow(form: SignForm, label: string, key: string): string {
  const entry = form.training[key];
  const current = entry?.use ?? "";
  const constraint =
    current === "constrained"
      ? `<input type="text" name="constraint:${key}" placeholder="Constraint details (required)" value="${esc(entry?.constraint_info ?? "")}" />`
      : "";
  return `
      <div class="usecase">
        <div class="label">${esc(label)}</div>
        ${useSegments(key, (v) => String(current === v))}
        ${constraint}
      </div>`;
}

/** The details field of the row for all uses: only while the section is closed and every use is constrained, and
 * only when they share their details; otherwise a pointer to the rows of their own. */
function allUsesConstraint(form: SignForm, uses: string[]): string {
  if (form.open.training || !uses.every((u) => u === "constrained")) return "";
  const details = new Set(USE_CASES.map((u) => form.training[u.key]?.constraint_info ?? ""));
  if (details.size > 1) return `<span class="hint">The constraint details differ per use; open the section to edit them.</span>`;
  const [shared] = details;
  return `<input type="text" name="constraint:${ALL_USES}" placeholder="Constraint details for all uses (required)" value="${esc(shared)}" />`;
}

/** The training and data mining section: its header holds the choice for all uses, which presses a choice all uses
 * share and marks one only some of them have as mixed; each use on its own while it is open. */
function trainingFieldset(form: SignForm): string {
  const uses = USE_CASES.map((u) => form.training[u.key]?.use ?? "");
  const pressed = (v: string) => {
    const n = uses.filter((u) => u === v).length;
    return n === uses.length ? "true" : n > 0 ? "mixed" : "false";
  };
  const all = useSegments(ALL_USES, pressed);
  const rows = form.open.training ? `<div>${USE_CASES.map((u) => useRow(form, u.label, u.key)).join("")}</div>` : "";
  return `
      <div class="fieldset fold">
        ${foldHeader("training", form.open.training, "Training and data mining", TRAINING_HELP, all)}
        ${allUsesConstraint(form, uses)}
        ${rows}
      </div>`;
}

/** Render the sign form. */
export function signTab(form: SignForm, inspection: Inspection, info: AppInfo | null, context: SignContext): string {
  const hasManifest = Boolean(inspection.manifest);
  const custom = form.credentials.kind === "custom" ? form.credentials : null;

  return `
    <form class="card form" id="sign-form" autocomplete="off">
      <div class="fieldset">
        <div class="legend">Manifest${helpTip(hasManifest ? "This file and its Content Credentials become the parent ingredient of a new manifest (c2pa.opened)." : "This file becomes the parent ingredient of a new manifest (c2pa.opened).")}</div>
        <div class="field">
          <label for="f-title">Title${helpTip("Feeds the Meta-Code and is stored in the manifest (cawg.metadata).")}</label>
          <input type="text" id="f-title" name="title" value="${esc(form.title)}" required />
        </div>
        <div class="field">
          <label for="f-description">Description <span class="hint">(optional)</span>${helpTip("Feeds the Meta-Code, together with the title.")}</label>
          <input type="text" id="f-description" name="description" value="${esc(form.description)}" />
        </div>
        <span class="hint" data-writes-metadata ${writesMetadata(form, inspection) ? "" : "hidden"}>${esc(WRITES_METADATA)}</span>
      </div>

      ${hasManifest ? "" : sourceFieldset(form.source, inspection.kind)}

      ${bindingFieldset(form, inspection, context.semantic)}

      ${trainingFieldset(form)}

      <div class="fieldset">
        ${settingLine(
          "f-cred",
          "Signing credentials",
          "The built-in demo certificate is the public c2pa-rs test key (ES256). It is on no C2PA trust list, so viewers show the signer as untrusted. Your own certificate chain and private key are PEM files.",
          `<select id="f-cred" name="cred"><option value="demo" ${custom ? "" : "selected"}>Built-in demo certificate</option><option value="custom" ${custom ? "selected" : ""}>My own certificate and key</option></select>`,
        )}
        ${
          custom
            ? `
        <div class="field">
          <label>Certificate chain (PEM)</label>
          <div class="row"><input type="text" class="mono" name="cert_path" value="${esc(custom.cert_path)}" readonly placeholder="choose a .pem or .pub file" /><button type="button" class="btn" data-pick="cert_path">Choose</button></div>
        </div>
        <div class="field">
          <label>Private key (PEM)</label>
          <div class="row"><input type="text" class="mono" name="key_path" value="${esc(custom.key_path)}" readonly placeholder="choose a .pem or .key file" /><button type="button" class="btn" data-pick="key_path">Choose</button></div>
        </div>
        <div class="field">
          <label for="f-alg">Signature algorithm</label>
          <select id="f-alg" name="alg">${SIGNING_ALGS.map((a) => `<option value="${a}" ${a === custom.alg ? "selected" : ""}>${a.toUpperCase()}</option>`).join("")}</select>
        </div>`
            : ""
        }
      </div>

      ${timestampFieldset(form.timestamp, info)}

      <div class="fieldset">
        <div class="legend">Output</div>
        <div class="field">
          <label>Signed copy is written to</label>
          <div class="row"><input type="text" class="mono" name="output" value="${esc(form.output)}" readonly /><button type="button" class="btn" data-pick="output">Choose</button></div>
        </div>
      </div>

      ${signActions(form, inspection, context)}
    </form>`;
}

/** Why the Sign button waits. */
export const WAITING = "Waiting for the units still being computed.";

const CHEVRON = `<svg class="chevron" viewBox="0 0 10 6" aria-hidden="true"><path d="M1 1l4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.4"/></svg>`;

/** The name of each signing action, as its button and menu entry say it. */
const MODE_LABEL: Record<SignMode, string> = {
  register: "Sign, embed &amp; register",
  embed: "Sign &amp; embed",
  sidecar: "Sign",
};

/** What signing in `mode` writes, in one line (HTML); for a sidecar, why there is none when it cannot be written. */
function writes(mode: SignMode, form: SignForm, inspection: Inspection): string {
  const out = splitPath(form.output);
  const copy = `<span class="mono">${esc(out.stem + out.ext)}</span>`;
  if (mode === "register") return `Writes ${copy} and registers its manifest on the ISCC test network.`;
  if (mode === "embed") return `Writes ${copy}.`;
  if (inspection.sidecar_block) return esc(inspection.sidecar_block);
  const { stem } = splitPath(inspection.path);
  return `Writes <span class="mono">${esc(stem)}.c2pa</span> next to <span class="mono">${esc(inspection.file_name)}</span>, which stays as it is.`;
}

/** One entry of the menu of signing actions; a sidecar that cannot be written stays listed, disabled, with why. */
function modeItem(mode: SignMode, form: SignForm, inspection: Inspection): string {
  const off = mode === "sidecar" && inspection.sidecar_block ? "disabled" : "";
  return `<button type="button" role="menuitem" data-sign="${mode}" ${off}><span class="t">${MODE_LABEL[mode]}</span><span class="d">${writes(mode, form, inspection)}</span></button>`;
}

/** The submit area: the reason a file cannot be signed in place of the buttons, or a warning above them when signing
 * does something to the file its owner may not want. The default action signs, embeds and registers; the arrow beside
 * it opens every action, each of which runs at once. The buttons wait while the background pass computes units of the
 * file (`analysing`). No button is a submit button: Enter in a field never signs, let alone registers. */
function signActions(form: SignForm, inspection: Inspection, context: SignContext): string {
  if (inspection.sign_block) {
    return `<div class="actions"><p class="banner error grow">${esc(inspection.sign_block)}</p></div>`;
  }
  const warning = inspection.sign_warning ? `<p class="banner warn">${esc(inspection.sign_warning)}</p>` : "";
  const off = context.analysing ? "disabled" : "";
  const menu = context.menu
    ? `<div class="menu" role="menu">${(["register", "embed", "sidecar"] as SignMode[]).map((m) => modeItem(m, form, inspection)).join("")}</div>`
    : "";
  return `
      ${warning}
      <div class="actions">
        <span class="msg" id="sign-msg">${context.analysing ? esc(WAITING) : writes("register", form, inspection)}</span>
        <div class="split">
          ${menu}
          <button type="button" class="btn primary main" id="sign-submit" data-sign="register" ${off}>${MODE_LABEL.register}</button>
          <button type="button" class="btn primary more" data-action="sign-menu" aria-label="More signing actions" aria-haspopup="menu" aria-expanded="${context.menu}" ${off}>${CHEVRON}</button>
        </div>
      </div>
      <p class="register-note">${esc(REGISTER_NOTE)}</p>`;
}
