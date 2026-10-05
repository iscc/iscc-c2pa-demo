// Application state, rendering and event wiring for the ISCC C2PA Demo.

import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open, save } from "@tauri-apps/plugin-dialog";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";

import {
  type AppInfo,
  type AssetKind,
  appInfo,
  cancelTasks,
  ffmpegStatus,
  type Inspection,
  initialPath,
  inspectAsset,
  installFfmpeg,
  metaCode,
  needsFfmpeg,
  type Progress,
  type SemanticKind,
  type SemanticSettings,
  semanticSettings,
  setSemantic,
  signAsset,
  type ToolStatus,
  type UnitSlug,
} from "./api";
import logoUrl from "./assets/iscc-logo-black-coral.svg";
import { copyText, esc, json, listJoin, splitPath } from "./util";
import { type Analysis, assetCard, patchPending, unitList } from "./views/asset";
import { credentialsTab, needsIsccBinding } from "./views/credentials";
import { type Busy, busyOverlay, downloadProgress, patchProgress, type ToolPrompt, toolOverlay } from "./views/overlay";
import { patchSettingsDownload, type SettingsDialog, type SettingsOperation, settingsOverlay } from "./views/settings";
import {
  dropFailedUnits,
  metaPreviewHint,
  newSignForm,
  type SignForm,
  setTrainingUse,
  signedMessages,
  signProblems,
  signTab,
  type TimestampChoice,
  toRequest,
  WAITING,
} from "./views/sign";
import { startScreen } from "./views/start";

type Tab = "credentials" | "sign" | "raw";

interface State {
  info: AppInfo | null;
  inspection: Inspection | null;
  form: SignForm | null;
  tab: Tab;
  busy: Busy | null;
  /** The background pass computing the open file's slow units (a video's frames and hashes, the Semantic-Code);
   * null when none runs. It stays, marked stopped, when the user stops it or it fails. */
  analysis: Analysis | null;
  /** The offer to download ffmpeg before a video is opened, and that download. */
  tool: ToolPrompt | null;
  /** Which Semantic-Codes are on; null until known, which counts as off. */
  semantic: SemanticSettings | null;
  /** The Settings dialog while it is open. */
  settings: SettingsDialog | null;
  error: string | null;
  banner: { text: string; path?: string; note?: string } | null;
  dragging: boolean;
}

const state: State = {
  info: null,
  inspection: null,
  form: null,
  tab: "credentials",
  busy: null,
  analysis: null,
  tool: null,
  semantic: null,
  settings: null,
  error: null,
  banner: null,
  dragging: false,
};

const appElement = document.getElementById("app");
if (!appElement) throw new Error("index.html has no #app element");
const app: HTMLElement = appElement;

/** Full render. Forms keep their values in `state.form`, so re-rendering is safe; the focused control keeps the
 * focus and its caret, so a render the user did not cause (a background pass finishing) does not interrupt typing. */
function render() {
  const { info, inspection } = state;
  const scroll = [...app.querySelectorAll<HTMLElement>(".column")].map((c) => c.scrollTop);
  const focus = focusedControl();
  // While a switch is being changed, nothing else may start or stop a task: see `switchSemantic`.
  const locked = state.settings?.operation ? "disabled" : "";
  const fileHtml = inspection
    ? `<div class="file"><span class="name" title="${esc(inspection.path)}">${esc(inspection.file_name)}</span><button class="btn small quiet" data-action="close" ${locked}>Close</button></div>`
    : "";
  const settingsOff = state.busy || state.tool || state.settings ? "disabled" : "";
  app.innerHTML = `
    <header class="topbar">
      <div class="brand">
        <img src="${logoUrl}" alt="ISCC" draggable="false" />
        <span class="divider"></span>
        <span class="product">C2PA Demo</span>
      </div>
      <span class="spacer"></span>
      ${fileHtml}
      <button class="btn small quiet" data-action="settings" ${settingsOff}>Settings</button>
      <button class="btn small" data-action="open" ${locked}>Open file</button>
      <span class="version mono">${info ? `v${esc(info.version)} · c2pa ${esc(info.c2pa_version)}` : ""}</span>
    </header>
    <main>
      ${inspection ? workspace(inspection) : startScreen(info, state.error, !(semanticOn("image") || semanticOn("text")))}
      <div class="dropoverlay ${state.dragging ? "active" : ""}"><div class="ring">Drop to inspect</div></div>
      ${state.busy ? busyOverlay(state.busy) : ""}
      ${state.tool ? toolOverlay(state.tool) : ""}
      ${state.settings ? settingsOverlay(state.settings, state.semantic) : ""}
    </main>`;
  app.querySelectorAll<HTMLElement>(".column").forEach((c, i) => {
    c.scrollTop = scroll[i] ?? 0;
  });
  restoreFocus(focus);
}

/** A form control found again after a render: by name, and by value for one of several checkboxes or radio buttons;
 * the caret of a text field. */
interface Focus {
  selector: string;
  start: number | null;
  end: number | null;
}

/** The named form control in the app that has the focus, if any. */
function focusedControl(): Focus | null {
  const el = document.activeElement;
  if (!(el instanceof HTMLInputElement || el instanceof HTMLSelectElement) || !el.name || !app.contains(el)) return null;
  const checkable = el instanceof HTMLInputElement && (el.type === "checkbox" || el.type === "radio");
  const selector = `[name="${CSS.escape(el.name)}"]${checkable ? `[value="${CSS.escape(el.value)}"]` : ""}`;
  const text = el instanceof HTMLInputElement && !checkable;
  return { selector, start: text ? el.selectionStart : null, end: text ? el.selectionEnd : null };
}

/** Give the focus, and the caret, back to the control `focus` names when the render kept it. */
function restoreFocus(focus: Focus | null) {
  const el = focus && app.querySelector<HTMLInputElement | HTMLSelectElement>(focus.selector);
  if (!focus || !el) return;
  el.focus({ preventScroll: true });
  if (el instanceof HTMLInputElement && focus.start !== null) el.setSelectionRange(focus.start, focus.end);
}

/** Says which bytes the left-hand units were computed from: always the whole file, as any ISCC tool computes them.
 * A sidecar manifest is not in the file. */
function unitListHint(inspection: Inspection): string {
  const embedded = inspection.manifest ? !inspection.manifest.sidecar : Boolean(inspection.manifest_error);
  return embedded ? "computed now, credentials included" : "computed now, 256 bit";
}

/** Whether the Semantic-Code of files of `kind` is on; unknown counts as off, and audio and video have none. */
function semanticOn(kind: AssetKind): boolean {
  return (kind === "image" || kind === "text") && Boolean(state.semantic?.[kind].on);
}

/** Whether the views show the open file's Semantic-Code: only when its kind is on and the inspection mentions the
 * unit (computed, pending, or with a reason). The switch hides what an inspection made before it was turned off; the
 * inspection hides what the backend left out while the switch here is stale (a model deleted by hand). */
function semanticShown(inspection: Inspection): boolean {
  const mentioned =
    inspection.iscc.some((u) => u.unit === "semantic") || inspection.pending.includes("semantic") || Boolean(inspection.semantic_error);
  return mentioned && semanticOn(inspection.kind);
}

/** True while the background pass runs, which signing waits for. */
function analysing(): boolean {
  return Boolean(state.analysis && !state.analysis.stopped);
}

function workspace(inspection: Inspection): string {
  const tabs: [Tab, string, string][] = [
    ["credentials", "Content Credentials", ""],
    ["sign", "Sign", needsIsccBinding(inspection) ? "This file has no ISCC soft binding yet" : ""],
    ["raw", "Manifest JSON", ""],
  ];
  const semantic = semanticShown(inspection);
  let panel: string;
  switch (state.tab) {
    case "sign":
      panel = state.form ? signTab(state.form, inspection, state.info, { analysing: analysing(), semantic }) : "";
      break;
    case "raw":
      panel = inspection.manifest_json
        ? `<section class="card"><header><h2>Manifest store</h2><span class="grow"></span><button class="btn small quiet" data-copy="${esc(json(inspection.manifest_json))}">Copy</button></header><div class="body"><pre class="json">${esc(json(inspection.manifest_json))}</pre></div></section>`
        : `<section class="card"><div class="center"><h3>No manifest store</h3><p>This file has no C2PA data to show.</p></div></section>`;
      break;
    default:
      panel = credentialsTab(inspection, semantic);
  }
  const banner = state.banner
    ? `<div class="banner"><span class="grow">${esc(state.banner.text)}</span>${state.banner.path ? `<button class="btn small" data-reveal="${esc(state.banner.path)}">Show in folder</button>` : ""}<button class="btn small quiet" data-action="dismiss">Dismiss</button></div>${state.banner.note ? `<div class="banner warn"><span class="grow">${esc(state.banner.note)}</span></div>` : ""}`
    : "";
  const error = state.error
    ? `<div class="banner error"><span class="grow">${esc(state.error)}</span><button class="btn small quiet" data-action="dismiss">Dismiss</button></div>`
    : "";
  return `
    <div class="workspace">
      <div class="column">
        ${assetCard(inspection)}
        ${unitList(inspection, { hint: unitListHint(inspection), analysis: state.analysis, semantic })}
      </div>
      <div class="column">
        ${banner}${error}
        <div>
          <div class="tabs" role="tablist">
            ${tabs.map(([id, label, nudge]) => `<button class="tab" role="tab" data-tab="${id}" aria-selected="${state.tab === id}"${nudge ? ` title="${nudge}"` : ""}>${label}${nudge ? `<span class="nudge" aria-label="${nudge}"></span>` : ""}</button>`).join("")}
          </div>
          <div class="tabpanel" role="tabpanel">${panel}</div>
        </div>
      </div>
    </div>`;
}

/** Sequence numbers of the latest load and signing; results of older ones are discarded. */
let loadSeq = 0;
let signSeq = 0;

/** What each stage of a signing is called on its progress card. */
const STAGE_LABEL: Partial<Record<Progress["stage"], string>> = {
  source: "Analysing the file to sign",
  write: "Writing the signed copy",
  output: "Checking the signed copy",
};

/** A spinner labelled `label`, until the task reports progress. */
function spinner(label: string): Busy {
  return { label, fraction: null, reporting: false, cancellable: false };
}

/** Show the progress of the signing that `busy` waits for: the first report of a stage renders its progress card,
 * later ones move the bar in place. A report arriving once another task has taken over the overlay is dropped. Only
 * the analysis of the source, which leaves no output behind, offers Cancel: writing the signed copy and checking it
 * do not. A signing without a progress card keeps its spinner while the copy is written. */
function showProgress(busy: Busy, p: Progress) {
  const label = STAGE_LABEL[p.stage];
  if (state.busy !== busy || !label || (p.stage === "write" && !busy.reporting)) return;
  busy.fraction = p.fraction;
  if (busy.reporting && busy.label === label) {
    patchProgress(app, p.fraction);
    return;
  }
  Object.assign(busy, { label, reporting: true, cancellable: p.stage === "source" });
  render();
}

/** The status of ffmpeg when opening `path` needs it and it can be installed. Null when the file needs none (no
 * video, or one with sound only), when ffmpeg is installed or has no build for this platform, or when either is
 * unknown: the inspection then says what is missing. */
async function missingFfmpeg(path: string): Promise<ToolStatus | null> {
  if (!(await needsFfmpeg(path).catch(() => false))) return null;
  const status = await ffmpegStatus().catch(() => null);
  return status?.available && !status.installed ? status : null;
}

/** Download ffmpeg as agreed, then open the video that needed it. */
async function installTool() {
  const prompt = state.tool;
  if (!prompt) return;
  prompt.download = { received: 0, total: prompt.status.bytes ?? 0 };
  render();
  try {
    await installFfmpeg((d) => {
      if (state.tool !== prompt) return;
      prompt.download = d;
      patchProgress(app, ...downloadProgress(d));
    });
    if (state.tool !== prompt) return;
    state.tool = null;
    await load(prompt.path);
  } catch (e) {
    if (state.tool !== prompt) return;
    state.tool = null;
    state.error = `ffmpeg could not be installed: ${e}`;
    render();
  }
}

/** Open the Settings dialog and read the switches afresh, so a model deleted by hand shows as off. */
async function openSettings() {
  const open: SettingsDialog = { operation: null, errors: {}, error: null };
  state.settings = open;
  render();
  try {
    state.semantic = await semanticSettings();
  } catch (e) {
    open.error = `The settings could not be read: ${e}`;
  }
  if (state.settings === open) render();
}

/** Close the Settings dialog, unless a switch is still being changed. */
function closeSettings() {
  if (!state.settings || state.settings.operation) return;
  state.settings = null;
  render();
}

/** Switch the Semantic-Code `kind` on or off and let the open file follow. Switching on downloads its model first
 * when it is missing. One change at a time: there is one task generation, which every download captures when it
 * starts, so a second change, Close or a switch-off that stops a pass would cancel the download too. */
async function switchSemantic(kind: SemanticKind, on: boolean) {
  const open = state.settings;
  if (!open || open.operation) return;
  const known = state.semantic?.[kind];
  const download = on && known && !known.installed ? { received: 0, total: known.bytes } : null;
  const operation: SettingsOperation = { kind, on, download, cancelled: false };
  open.operation = operation;
  delete open.errors[kind];
  render();
  try {
    state.semantic = await setSemantic(kind, on, (d) => {
      if (open.operation !== operation) return;
      const first = !operation.download;
      operation.download = d;
      if (first) render();
      else patchSettingsDownload(app, d);
    });
    open.operation = null;
    semanticChanged(kind);
  } catch (e) {
    open.operation = null;
    if (!operation.cancelled) open.errors[kind] = String(e);
  }
  render();
}

/** Stop the model's download. The one task generation stops a background pass too, which then shows as stopped,
 * with Resume. */
function cancelSettings() {
  const operation = state.settings?.operation;
  if (!operation?.download) return;
  operation.cancelled = true;
  if (state.analysis) state.analysis.stopped = true;
  void cancelTasks().catch((e) => console.error(e));
  render();
}

/** Let the open file follow a switch of the Semantic-Code `kind`, if it is of that kind. */
function semanticChanged(kind: SemanticKind) {
  const inspection = state.inspection;
  if (!inspection || inspection.kind !== kind) return;
  syncForm();
  if (semanticOn(kind)) semanticSwitchedOn(inspection);
  else semanticSwitchedOff(inspection);
}

/** The Semantic-Code switched on: computed in the background and ticked in the sign form. A document without text
 * has none, for the reason it has no Content-Code. */
function semanticSwitchedOn(inspection: Inspection) {
  if (inspection.content_error) {
    inspection.semantic_error = inspection.content_error;
    return;
  }
  inspection.semantic_error = null;
  if (!inspection.pending.includes("semantic")) inspection.pending = [...inspection.pending, "semantic"];
  state.form?.units.add("semantic");
  void analyse(inspection.path);
}

/** The Semantic-Code switched off: gone at once from the unit list and the sign form, without recomputing anything.
 * A background pass that only computed it stops; one that computes more finishes, and the views hide the
 * Semantic-Code it brings back. */
function semanticSwitchedOff(inspection: Inspection) {
  inspection.iscc = inspection.iscc.filter((u) => u.unit !== "semantic");
  inspection.pending = inspection.pending.filter((u) => u !== "semantic");
  inspection.semantic_error = null;
  state.form?.units.delete("semantic");
  if (!state.analysis || inspection.pending.length > 0) return;
  if (!state.analysis.stopped) void cancelTasks().catch((e) => console.error(e));
  state.analysis = null;
}

/** Compute the slow units of the file open now in the background: the inspection shows at once, each pending unit
 * shows its progress, and the full result replaces the inspection, keeping the tab, the scroll offsets and the sign
 * form. A newer load or Close drops the result; Stop or a failure keeps the pass, marked stopped, so it can be
 * resumed. */
async function analyse(path: string) {
  const seq = loadSeq;
  const analysis: Analysis = { progress: {}, stopped: false };
  state.analysis = analysis;
  render();
  try {
    const full = await inspectAsset(path, true, (p) => {
      if (state.analysis !== analysis) return;
      analysis.progress[p.stage as UnitSlug] = p.fraction;
      patchPending(app, p.stage as UnitSlug, p.fraction);
    });
    if (seq !== loadSeq || state.analysis !== analysis) return;
    syncForm();
    state.inspection = full;
    state.analysis = null;
    if (state.form) dropFailedUnits(state.form, full);
  } catch (e) {
    if (seq !== loadSeq || state.analysis !== analysis) return;
    if (!analysis.stopped) {
      analysis.stopped = true;
      state.error = `The analysis failed: ${e}`;
    }
  }
  render();
}

/** Load and inspect a file path. A video with frames needs ffmpeg for its Meta-Code and Content-Code; when it is
 * missing, the user is offered the download first, unless `offerFfmpeg` is false. The file opened last wins: it
 * replaces whatever load or signing still runs. Nothing opens while a tool downloads or a switch in Settings is
 * being changed, since opening cancels the tasks that run. Opening closes the ffmpeg offer and Settings: a switch
 * flipped while the file is read would act on the file open before it. */
async function load(path: string, offerFfmpeg = true) {
  if (state.tool?.download || state.settings?.operation) return;
  state.tool = null;
  state.settings = null;
  const ext = splitPath(path).ext.slice(1).toLowerCase();
  if (state.info && !state.info.extensions.includes(ext)) {
    const kinds = listJoin(state.info.kinds.map((k) => `${k.label.toLowerCase()} (${k.extensions.join(", ")})`));
    state.error = `Unsupported file type ".${ext}". Supported are ${kinds}.`;
    render();
    return;
  }
  // Taken before the first wait, so that a file opened later, or Close, overtakes this load wherever it is.
  const seq = ++loadSeq;
  const missing = offerFfmpeg ? await missingFfmpeg(path) : null;
  // An analysis still running for the previous file stops first; awaited, so the
  // cancellation cannot reach the inspection started next.
  if (seq === loadSeq && (state.busy || analysing())) await cancelTasks().catch((e) => console.error(e));
  if (seq !== loadSeq) return;
  state.analysis = null;
  // A signing still running is given up with the file it belongs to.
  signSeq++;
  state.error = null;
  if (missing) {
    state.busy = null;
    state.tool = { path, status: missing, download: null };
    return render();
  }
  if (await glance(seq, path)) void analyse(path);
}

/** Show the file at `path` at a glance, its slow units pending; true when the load `seq` is still the latest and
 * units are pending, for the background pass. */
async function glance(seq: number, path: string): Promise<boolean> {
  state.busy = spinner("Inspecting");
  render();
  try {
    // The switches as the backend applies them now: a model may have come or gone since they were read.
    state.semantic = await semanticSettings().catch(() => state.semantic);
    const inspection = await inspectAsset(path, false, () => {});
    if (seq !== loadSeq) return false;
    state.inspection = inspection;
    state.tab = "credentials";
    state.banner = null;
    await resetSignForm(inspection);
    return inspection.pending.length > 0 && seq === loadSeq;
  } catch (e) {
    if (seq === loadSeq) state.error = String(e);
    return false;
  } finally {
    if (seq === loadSeq) {
      state.busy = null;
      render();
    }
  }
}

/** Ask the backend for the Meta-Code of the current title and description. */
async function refreshMetaPreview() {
  const form = state.form;
  if (!form) return;
  const title = form.title.trim();
  const meta = state.inspection?.meta_fields.meta ?? undefined;
  form.metaPreview = null;
  form.metaError = null;
  if (!title) return;
  try {
    form.metaPreview = await metaCode(title, form.description.trim() || undefined, meta);
  } catch (e) {
    form.metaError = String(e);
  }
}

/** Fresh sign form with its Meta-Code preview. The Meta-Code starts unticked when the file's own
 * title and ISCC metadata cannot produce one, or its tags were not read (a video without ffmpeg);
 * a title can still be typed in. The Content-Code starts unticked when the file has none (audio
 * too short). */
async function resetSignForm(inspection: Inspection) {
  const form = newSignForm(inspection, state.info?.tsa_presets ?? []);
  state.form = form;
  await refreshMetaPreview();
  if (form.metaError || inspection.meta_error) form.units.delete("meta");
  if (inspection.content_error) form.units.delete(inspection.kind);
}

/** Update only the Meta-Code preview line inside the form, preserving focus. */
function patchMetaPreview() {
  const el = app.querySelector<HTMLElement>("[data-meta-code]");
  const code = state.form?.metaPreview?.iscc;
  if (!el) return;
  if (code) {
    const [prefix, body] = code.split(/:(.*)/s);
    el.innerHTML = `<span class="prefix">${esc(prefix)}:</span>${esc(body)}`;
    el.style.color = "";
  } else {
    el.textContent = state.form ? metaPreviewHint(state.form) : "";
    el.style.color = "var(--muted)";
  }
}

async function pickFile() {
  const info = state.info;
  const filters = info
    ? [
        { name: listJoin(info.kinds.map((k) => k.label)), extensions: info.extensions },
        ...info.kinds.map((k) => ({ name: k.label, extensions: k.extensions })),
      ]
    : [];
  const selected = await open({ multiple: false, directory: false, filters });
  if (typeof selected === "string") await load(selected);
}

async function pickPem(field: "cert_path" | "key_path") {
  const selected = await open({
    multiple: false,
    directory: false,
    filters: [{ name: "PEM", extensions: ["pem", "pub", "key", "crt", "cer"] }],
  });
  if (typeof selected === "string" && state.form && state.form.credentials.kind === "custom") {
    state.form.credentials[field] = selected;
    render();
  }
}

async function pickOutput() {
  const form = state.form;
  if (!form) return;
  const { ext, sep, dir, stem } = splitPath(form.output);
  const chosen = await save({
    defaultPath: form.output || `${dir}${sep}${stem}${ext}`,
    filters: [{ name: ext.slice(1).toUpperCase() || "File", extensions: [ext.slice(1) || "jpg"] }],
  });
  if (chosen) {
    form.output = chosen;
    render();
  }
}

async function submitSign() {
  const form = state.form;
  const inspection = state.inspection;
  if (!form || !inspection) return;
  const msg = app.querySelector<HTMLElement>("#sign-msg");
  const problems = analysing() ? [WAITING] : signProblems(form, inspection);
  if (problems.length > 0) {
    if (msg) {
      msg.textContent = problems.join(" ");
      msg.className = "msg error";
    }
    return;
  }
  const seq = ++signSeq;
  const busy = spinner("Signing");
  state.busy = busy;
  state.error = null;
  render();
  try {
    const result = await signAsset(toRequest(form, inspection, semanticShown(inspection)), (p) => {
      if (seq === signSeq) showProgress(busy, p);
    });
    if (seq !== signSeq) return;
    state.inspection = result.inspection;
    state.analysis = null;
    state.tab = "credentials";
    const { text, note } = signedMessages(result);
    state.banner = { text, path: result.output, note: note ?? undefined };
    await resetSignForm(result.inspection);
  } catch (e) {
    if (seq === signSeq) {
      state.error = `Signing failed: ${e}`;
      // A model deleted by hand turns its Semantic-Code off: read the switches again, so the form drops the unit
      // and the next Sign works.
      state.semantic = await semanticSettings().catch(() => state.semantic);
    }
  } finally {
    if (seq === signSeq) {
      state.busy = null;
      render();
    }
  }
  // A Semantic-Code left out at signing is computed for the copy as for any file opened.
  const copy = state.inspection;
  if (seq === signSeq && copy?.pending.length) void analyse(copy.path);
}

/** Read every form control into `state.form` so a re-render restores it. */
function syncForm() {
  const form = state.form;
  const el = app.querySelector<HTMLFormElement>("#sign-form");
  if (!form || !el) return;
  const data = new FormData(el);
  form.title = String(data.get("title") ?? "");
  form.description = String(data.get("description") ?? "");
  form.sourceType = String(data.get("source_type") ?? form.sourceType);
  form.units = new Set(data.getAll("unit").map(String));
  for (const key of Object.keys(form.training)) {
    const info = data.get(`constraint:${key}`);
    if (info !== null) form.training[key].constraint_info = String(info);
  }
  const cred = String(data.get("cred") ?? "demo");
  if (cred === "custom") {
    const prev = form.credentials.kind === "custom" ? form.credentials : { cert_path: "", key_path: "", alg: "es256" };
    form.credentials = { kind: "custom", cert_path: prev.cert_path, key_path: prev.key_path, alg: String(data.get("alg") ?? prev.alg) };
  } else {
    form.credentials = { kind: "demo" };
  }
  form.timestamp = readTimestamp(data, form.timestamp);
}

/** The timestamp choice in `data`; disabled controls are missing there and keep `prev`. */
function readTimestamp(data: FormData, prev: TimestampChoice): TimestampChoice {
  return {
    on: data.has("ts_on"),
    preset: String(data.get("ts_preset") ?? prev.preset),
    custom: String(data.get("ts_custom") ?? prev.custom),
  };
}

/** Handle a click on an element carrying one of the `data-*` hooks of the markup. */
async function onClick(target: HTMLElement) {
  const d = target.dataset;
  if (d.action) return onAction(d.action);
  if (d.tab) {
    syncForm();
    state.tab = d.tab as Tab;
    return render();
  }
  if (d.goto) {
    state.tab = d.goto as Tab;
    return render();
  }
  if (d.copy !== undefined) return copyWithFeedback(target, d.copy);
  if (d.pick) {
    syncForm();
    if (d.pick === "output") return void pickOutput();
    return void pickPem(d.pick as "cert_path" | "key_path");
  }
  if (d.reveal) return void revealItemInDir(d.reveal).catch((e) => console.error(e));
  if (d.useKey && state.form) {
    syncForm();
    setTrainingUse(state.form, d.useKey, d.use ?? "");
    return render();
  }
}

/** Stop the running analysis or download in the backend and forget about its result. */
function cancelRunning() {
  void cancelTasks().catch((e) => console.error(e));
  loadSeq++;
  signSeq++;
  state.busy = null;
  state.analysis = null;
  state.tool = null;
}

/** Stop the background pass; its pending units say so and offer to resume it. */
function stopAnalysis() {
  if (!state.analysis) return;
  state.analysis.stopped = true;
  void cancelTasks().catch((e) => console.error(e));
  render();
}

/** Top-bar, banner, unit list and overlay actions: open a file, close it, dismiss the messages, download ffmpeg, open
 * a video without it, stop or resume the background pass, open and close Settings, cancel. */
function onAction(action: string) {
  if (action === "open") return void pickFile();
  if (action === "install-tool") return void installTool();
  if (action === "skip-tool" && state.tool) return void load(state.tool.path, false);
  if (action === "stop-analysis") return stopAnalysis();
  if (action === "resume-analysis" && state.inspection) return void analyse(state.inspection.path);
  if (action === "settings") return void openSettings();
  if (action === "close-settings") return closeSettings();
  if (action === "cancel-settings") return cancelSettings();
  if (action === "cancel-task" || action === "cancel-tool") {
    cancelRunning();
    return render();
  }
  if (action === "close") {
    cancelRunning();
    state.inspection = null;
    state.form = null;
  }
  if (action === "close" || action === "dismiss") {
    state.banner = null;
    state.error = null;
    render();
  }
}

/** Copy `text` and let the button say for a moment whether that worked. */
async function copyWithFeedback(button: HTMLElement, text: string) {
  const ok = await copyText(text);
  const label = button.textContent;
  button.textContent = ok ? "Copied" : "Select and copy";
  window.setTimeout(() => {
    button.textContent = label;
  }, 1200);
}

let metaTimer: number | undefined;

function wireEvents() {
  app.addEventListener("click", async (ev) => {
    // External links open in the system browser; the web view itself never navigates away.
    const link = (ev.target as HTMLElement).closest<HTMLAnchorElement>("a.ext");
    if (link) {
      ev.preventDefault();
      if (/^https?:\/\//i.test(link.href)) void openUrl(link.href).catch((e) => console.error(e));
      return;
    }
    const target = (ev.target as HTMLElement).closest<HTMLElement>(
      "[data-action],[data-tab],[data-copy],[data-goto],[data-pick],[data-reveal],[data-use-key]",
    );
    if (target) await onClick(target);
  });

  app.addEventListener("input", (ev) => {
    const target = ev.target as HTMLInputElement;
    if (!target.closest("#sign-form")) return;
    syncForm();
    if (target.name === "title" || target.name === "description") {
      window.clearTimeout(metaTimer);
      metaTimer = window.setTimeout(async () => {
        await refreshMetaPreview();
        patchMetaPreview();
      }, 150);
    }
  });

  app.addEventListener("change", (ev) => {
    const target = ev.target as HTMLInputElement;
    if (target.name === "semantic") return void switchSemantic(target.value as SemanticKind, target.checked);
    if (["cred", "alg", "ts_on", "ts_preset"].includes(target.name)) {
      syncForm();
      render();
    }
  });

  app.addEventListener("submit", (ev) => {
    ev.preventDefault();
    syncForm();
    void submitSign();
  });

  app.addEventListener("keydown", (ev) => {
    const target = ev.target as HTMLElement;
    if ((ev.key === "Enter" || ev.key === " ") && target.matches(".dropzone")) {
      ev.preventDefault();
      void pickFile();
    }
  });

  // Escape closes Settings, wherever the focus is; it waits like Done while a switch is being changed.
  window.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape" && state.settings) closeSettings();
  });

  // Block the web view's default file navigation; Tauri delivers paths through its own event.
  window.addEventListener("dragover", (e) => e.preventDefault());
  window.addEventListener("drop", (e) => e.preventDefault());
}

async function wireDragDrop() {
  await getCurrentWebview().onDragDropEvent((event) => {
    const payload = event.payload;
    if (payload.type === "enter" || payload.type === "over") {
      if (!state.dragging) {
        state.dragging = true;
        app.querySelector(".dropoverlay")?.classList.add("active");
      }
    } else if (payload.type === "leave") {
      state.dragging = false;
      app.querySelector(".dropoverlay")?.classList.remove("active");
    } else if (payload.type === "drop") {
      state.dragging = false;
      const first = payload.paths[0];
      if (first) void load(first);
      else render();
    }
  });
}

async function main() {
  wireEvents();
  render();
  state.info = await appInfo().catch(() => null);
  state.semantic = await semanticSettings().catch(() => null);
  render();
  await wireDragDrop();
  const path = await initialPath().catch(() => null);
  if (path) await load(path);
}

void main();
