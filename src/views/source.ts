// Sign tab, "How was this … made?": two plain questions as dropdowns (or the full IPTC list) and the term they record on the
// parent ingredient of a source without Content Credentials.

import type { AssetKind } from "../api";
import { AFTER, chosenCode, type OfferedOrigin, originsFor, type SourceChoice, TERMS, termOf } from "../sourcetype";
import { esc, fullStop } from "../util";
import { helpTip } from "./icons";

/** What the questions call a file of `kind`. */
function noun(kind: AssetKind): string {
  return kind === "text" ? "document" : kind;
}

/** The fieldset asking how the file was made, with the term the answers record under it. */
export function sourceFieldset(choice: SourceChoice, kind: AssetKind): string {
  const listed = choice.term !== null;
  return `
      <div class="fieldset source">
        <div class="legend">How was this ${noun(kind)} made?${helpTip("Recorded as the digitalSourceType of the parent ingredient, one of the IPTC digital source types.")}</div>
        ${listed ? termList(choice.term ?? "") : questions(choice, kind)}
        ${recorded(chosenCode(kind, choice), listed)}
      </div>`;
}

/** The two questions as dropdowns; the second only once the first is answered with an origin that asks it. */
function questions(choice: SourceChoice, kind: AssetKind): string {
  const origins = originsFor(kind);
  const current = origins.find((o) => o.id === choice.origin);
  const options = [option("", "Don't say", "", !current), ...origins.map((o) => option(o.id, o.label, o.hint, o === current))].join("");
  const after = current?.asksAfter ? afterQuestion(choice.after, kind) : "";
  return `
        <div class="field">
          <label for="f-source-origin">Where does it come from?</label>
          <select id="f-source-origin" name="source_origin">${options}</select>
          ${current ? variantSelect(current, choice.variant) : ""}
        </div>
        ${after}`;
}

/** The variants of the chosen origin, as a second dropdown under it; nothing when it has only one. */
function variantSelect(origin: OfferedOrigin, variant: string): string {
  if (origin.variants.length < 2) return "";
  const picked = origin.variants.find((v) => v.id === variant) ?? origin.variants[0];
  const options = origin.variants.map((v) => option(v.id, v.label, "", v === picked)).join("");
  return `<select name="source_variant" aria-label="${esc(origin.label)}, more precisely">${options}</select>`;
}

/** The second question, with examples for files of `kind`. */
function afterQuestion(after: string, kind: AssetKind): string {
  const options = AFTER.map((a) => option(a.id, a.label, a.hints[kind] ?? "", a.id === after)).join("");
  return `
        <div class="field">
          <label for="f-source-after">What happened to it afterwards?</label>
          <select id="f-source-after" name="source_after">${options}</select>
        </div>`;
}

/** A dropdown option with its label and, after it, examples. */
function option(value: string, label: string, hint: string, selected: boolean): string {
  const text = hint ? `${label} · ${hint}` : label;
  return `<option value="${esc(value)}" ${selected ? "selected" : ""}>${esc(text)}</option>`;
}

/** Every active IPTC term in its group, by IPTC's name. */
function termList(term: string): string {
  const groups = [...new Set(TERMS.map((t) => t.group))].map((group) => {
    const options = TERMS.filter((t) => t.group === group)
      .map((t) => `<option value="${t.code}" ${t.code === term ? "selected" : ""}>${esc(t.name)}</option>`)
      .join("");
    return `<optgroup label="${esc(group)}">${options}</optgroup>`;
  });
  return `<select name="source_term" aria-label="Digital source type"><option value="" ${term ? "" : "selected"}>Not specified</option>${groups.join("")}</select>`;
}

/** The term the choice records, IPTC's name and definition of it, and the switch between the questions and the list. */
function recorded(code: string | null, listed: boolean): string {
  const toggle = `<button type="button" class="linkbtn" data-source-list="${listed ? "questions" : "list"}">${listed ? "answer the questions instead" : "choose the term yourself"}</button>`;
  const t = code ? termOf(code) : undefined;
  if (!t) {
    return `<p class="recorded"><span class="line">Nothing is recorded${helpTip("The C2PA specification lets an ingredient leave it out.")}</span>${toggle}</p>`;
  }
  const ai = t.ai ? `<span class="ai">Content Credentials viewers show such files as made with generative AI.</span>` : "";
  return `
        <p class="recorded">
          <span class="line">Recorded as <span class="term">${esc(t.code)}</span>${helpTip(`${t.name}. ${t.definition}${fullStop(t.definition)}`)}</span>
          ${ai}${toggle}
        </p>`;
}
