// ISCC test network card of a file with Content Credentials: register its manifest store on the test network (upload
// it to the test store, which declares its ISCC), and what the store answered. Registering again is harmless: the store
// answers the same bytes with the record it keeps.

import type { Registration } from "../api";
import { esc } from "../util";
import { REGISTER_NOTE } from "./sign";

/** A registration of the open file's manifest store, running, answered or failed. */
export type RegisterState =
  | { path: string; state: "running" }
  | { path: string; state: "done"; result: Registration }
  | { path: string; state: "failed"; reason: string };

/** What the store's declaration status means for finding the manifest. */
const DECLARATION_TEXT: Record<string, string> = {
  declared: "Registered. Others can find it on the test network in about a minute.",
  pending: "Registered on the test store, which finds it now. Its declaration on the network is retried in the background.",
  skipped:
    "Stored, not declared: the network needs a Data-Code and an Instance-Code in the ISCC soft binding. Only the test store finds it.",
  failed: "Stored, but the network refused its declaration. Only the test store finds it.",
  disabled: "Stored; the test store declares nothing at the moment. Only the test store finds it.",
  withdrawn: "Stored; its declaration was withdrawn. Only the test store finds it.",
};

/** The store's answer: what it means, the manifest's address and the declaration's ISCC-ID. */
function answer(result: Registration): string {
  const d = result.declaration;
  const text = DECLARATION_TEXT[d.status] ?? `Stored; declaration ${d.status}.`;
  const iscc = d.iscc_id
    ? `<dt>ISCC-ID</dt><dd class="mono">${esc(d.iscc_id)}${d.hub ? `<span class="hint">on ${esc(d.hub)}</span>` : ""}</dd>`
    : "";
  return `
    <p>${esc(text)}</p>
    <dl class="kv">
      <dt>Manifest</dt><dd class="mono"><a class="ext" href="${esc(result.manifest_url)}">${esc(result.manifest_id)}</a></dd>
      ${iscc}
    </dl>`;
}

/** The card for `registration`, the open file's, or null before any. */
export function registerCard(registration: RegisterState | null): string {
  const button = (label: string) => `<button class="btn small" data-action="register">${label}</button>`;
  let action = button("Register manifest");
  let body = `<p class="hint">${esc(REGISTER_NOTE)}</p>`;
  if (registration?.state === "running") {
    action = `<span class="status"><span class="spinner"></span>Registering…</span>`;
  } else if (registration?.state === "failed") {
    action = button("Try again");
    body = `<p>Registering failed: ${esc(registration.reason)}</p>${body}`;
  } else if (registration?.state === "done") {
    action = "";
    body = answer(registration.result);
  }
  return `
    <section class="card register">
      <header><h2>ISCC test network</h2><span class="grow"></span>${action}</header>
      <div class="body">${body}</div>
    </section>`;
}
