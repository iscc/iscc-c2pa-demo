// Content Credentials tab of a file without them: a compact card saying so, with the search of the ISCC test network
// for credentials that match the file's ISCC in its states (not started or searching, found, nothing found,
// unreachable), and what it found below it (found.ts). Only the ISCC leaves the machine, never the file.

import type { Inspection, Lookup } from "../api";
import { esc, listJoin } from "../util";
import { foundList } from "./found";
import { helpTip } from "./icons";

/** A search of the test network for the open file's credentials; once done, the store ids of the expanded matches. */
export type Search = { state: "searching" } | { state: "done"; lookup: Lookup; open: string[] } | { state: "failed"; reason: string };

/** The services asked, and the search service the resolver asks in turn. */
const HOSTS = ["c2pa-store-test.iscc.io", "c2pa-test.iscc.io"];
const AGGREGATOR = "search-test.iscc.id";

/** What the card leaves out: what the search does. */
const ABOUT =
  "This file carries no C2PA manifest. Recovery looks on the ISCC test network for Content Credentials whose ISCC soft binding matches this file or a near-identical one, and checks each here against this file.";

/** What leaves the machine, said before the first search (HTML). */
const PRIVACY = `Sends this file's ISCC, which identifies this exact file, with your IP address to ${listJoin(HOSTS.map(host))} (which asks ${host(AGGREGATOR)}). Not the file.`;

/** A host name that never breaks at its hyphens. */
function host(name: string): string {
  return `<span class="host">${esc(name)}</span>`;
}

const SEARCH_LABEL = "Recover Content Credentials";

/** The search button, disabled until the file's ISCC is computed. */
function searchButton(ready: boolean): string {
  return `<button class="btn primary" ${ready ? `data-action="search"` : "disabled"}>${SEARCH_LABEL}</button>`;
}

/** Why every service failed to answer; null when one did. */
function unanswered(search: Search | null): string | null {
  if (search?.state !== "done" || search.lookup.failures.length < HOSTS.length) return null;
  return listJoin(search.lookup.failures.map((f) => `${f.host}: ${f.reason}`));
}

/** Buttons and a short status line, per state. */
function stateRow(search: Search | null, ready: boolean): string {
  const sign = `<button class="btn" data-goto="sign">Sign this file</button>`;
  const again = `<button class="btn quiet" data-action="search">Recover again</button>`;
  if (search?.state === "searching") {
    return `<span class="status"><span class="spinner"></span>Recovering Content Credentials…</span>`;
  }
  const tryAgain = `<button class="btn primary" data-action="search">Try again</button>${sign}`;
  if (search?.state === "failed") return `${tryAgain}<span class="status">Recovery failed: ${esc(search.reason)}</span>`;
  const reason = unanswered(search);
  if (reason !== null) return `${tryAgain}<span class="status" title="${esc(reason)}">The test network did not answer.</span>`;
  if (search?.state === "done" && search.lookup.matches.length + search.lookup.related.length > 0) return `${sign}${again}`;
  if (search?.state === "done") {
    return `<button class="btn primary" data-goto="sign">Sign this file</button>${again}<span class="status">Nothing found</span>`;
  }
  return `${searchButton(ready)}${sign}`;
}

/** The tab of a file without Content Credentials: the compact card with the search's buttons and status, before the
 * first search what it sends, and what it found below. `ready` tells whether the file's ISCC is computed; `semantic`
 * whether its Semantic-Code is shown. */
export function searchTab(search: Search | null, ready: boolean, inspection: Inspection, semantic: boolean): string {
  const found = search?.state === "done" ? foundList(search.lookup, search.open, inspection, semantic) : "";
  const privacy = search === null ? `<p class="privacy">${PRIVACY}</p>` : "";
  return `
    <section class="card nocreds">
      <h3>No embedded Content Credentials${helpTip(ABOUT)}</h3>
      <div class="buttons">${stateRow(search, ready)}</div>
      ${privacy}
    </section>
    ${found}`;
}
