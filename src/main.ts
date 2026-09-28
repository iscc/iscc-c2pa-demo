// Application state, rendering and event wiring for the ISCC C2PA Demo.

import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open, save } from "@tauri-apps/plugin-dialog";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";

import { type AppInfo, appInfo, type Inspection, initialPath, inspectAsset, metaCode, signAsset } from "./api";
import logoUrl from "./assets/iscc-logo-black-coral.svg";
import { copyText, esc, json, listJoin, splitPath } from "./util";
import { assetCard, unitList } from "./views/asset";
import { credentialsTab, needsIsccBinding } from "./views/credentials";
import {
  metaPreviewHint,
  newSignForm,
  type SignForm,
  setTrainingUse,
  signedMessages,
  signProblems,
  signTab,
  type TimestampChoice,
  toRequest,
} from "./views/sign";
import { startScreen } from "./views/start";

type Tab = "credentials" | "sign" | "raw";

interface State {
  info: AppInfo | null;
  inspection: Inspection | null;
  form: SignForm | null;
  tab: Tab;
  busy: string | null;
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
  error: null,
  banner: null,
  dragging: false,
};

const appElement = document.getElementById("app");
if (!appElement) throw new Error("index.html has no #app element");
const app: HTMLElement = appElement;

/** Full render. Forms keep their values in `state.form`, so re-rendering is safe. */
function render() {
  const { info, inspection } = state;
  const scroll = [...app.querySelectorAll<HTMLElement>(".column")].map((c) => c.scrollTop);
  const fileHtml = inspection
    ? `<div class="file"><span class="name" title="${esc(inspection.path)}">${esc(inspection.file_name)}</span><button class="btn small quiet" data-action="close">Close</button></div>`
    : "";
  app.innerHTML = `
    <header class="topbar">
      <div class="brand">
        <img src="${logoUrl}" alt="ISCC" draggable="false" />
        <span class="divider"></span>
        <span class="product">C2PA Demo</span>
      </div>
      <span class="spacer"></span>
      ${fileHtml}
      <button class="btn small" data-action="open">Open file</button>
      <span class="version mono">${info ? `v${esc(info.version)} · c2pa ${esc(info.c2pa_version)}` : ""}</span>
    </header>
    <main>
      ${inspection ? workspace(inspection) : startScreen(info, state.error)}
      <div class="dropoverlay ${state.dragging ? "active" : ""}"><div class="ring">Drop to inspect</div></div>
      ${state.busy ? `<div class="busy"><div class="ring" role="status" aria-label="${esc(state.busy)}"></div></div>` : ""}
    </main>`;
  app.querySelectorAll<HTMLElement>(".column").forEach((c, i) => {
    c.scrollTop = scroll[i] ?? 0;
  });
}

/** Says which bytes the left-hand units were computed from: always the whole file, as any ISCC tool computes them. */
function unitListHint(inspection: Inspection): string {
  return inspection.manifest || inspection.manifest_error ? "computed now, credentials included" : "computed now, 256 bit";
}

function workspace(inspection: Inspection): string {
  const tabs: [Tab, string, string][] = [
    ["credentials", "Content Credentials", ""],
    ["sign", "Sign", needsIsccBinding(inspection) ? "This file has no ISCC soft binding yet" : ""],
    ["raw", "Manifest JSON", ""],
  ];
  let panel: string;
  switch (state.tab) {
    case "sign":
      panel = state.form ? signTab(state.form, inspection, state.info) : "";
      break;
    case "raw":
      panel = inspection.manifest_json
        ? `<section class="card"><header><h2>Manifest store</h2><span class="grow"></span><button class="btn small quiet" data-copy="${esc(json(inspection.manifest_json))}">Copy</button></header><div class="body"><pre class="json">${esc(json(inspection.manifest_json))}</pre></div></section>`
        : `<section class="card"><div class="center"><h3>No manifest store</h3><p>This file has no C2PA data to show.</p></div></section>`;
      break;
    default:
      panel = credentialsTab(inspection);
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
        ${unitList(inspection, unitListHint(inspection))}
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

/** Sequence number of the latest load; results of older loads are discarded. */
let loadSeq = 0;

/** Load and inspect a file path. */
async function load(path: string) {
  const ext = splitPath(path).ext.slice(1).toLowerCase();
  if (state.info && !state.info.extensions.includes(ext)) {
    const kinds = listJoin(state.info.kinds.map((k) => `${k.label.toLowerCase()} (${k.extensions.join(", ")})`));
    state.error = `Unsupported file type ".${ext}". Supported are ${kinds}.`;
    render();
    return;
  }
  const seq = ++loadSeq;
  state.busy = "Inspecting";
  state.error = null;
  render();
  try {
    const inspection = await inspectAsset(path);
    if (seq !== loadSeq) return;
    state.inspection = inspection;
    state.tab = "credentials";
    state.banner = null;
    await resetSignForm(inspection);
  } catch (e) {
    if (seq !== loadSeq) return;
    state.error = String(e);
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
 * title and ISCC metadata cannot produce one (a missing title can still be typed in), the
 * Content-Code when the file has none (audio too short). */
async function resetSignForm(inspection: Inspection) {
  const form = newSignForm(inspection, state.info?.tsa_presets ?? []);
  state.form = form;
  await refreshMetaPreview();
  if (form.metaError) form.units.delete("meta");
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
  const problems = signProblems(form, inspection);
  if (problems.length > 0) {
    if (msg) {
      msg.textContent = problems.join(" ");
      msg.className = "msg error";
    }
    return;
  }
  state.busy = "Signing";
  state.error = null;
  render();
  try {
    const result = await signAsset(toRequest(form, inspection));
    state.inspection = result.inspection;
    state.tab = "credentials";
    const { text, note } = signedMessages(result);
    state.banner = { text, path: result.output, note: note ?? undefined };
    await resetSignForm(result.inspection);
  } catch (e) {
    state.error = `Signing failed: ${e}`;
  } finally {
    state.busy = null;
    render();
  }
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

/** Top-bar and banner actions: open a file, close it, dismiss the messages. */
function onAction(action: string) {
  if (action === "open") return void pickFile();
  if (action === "close") {
    loadSeq++;
    state.busy = null;
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
  render();
  await wireDragDrop();
  const path = await initialPath().catch(() => null);
  if (path) await load(path);
}

void main();
