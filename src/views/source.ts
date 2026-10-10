// Sign tab, "How was this … made?": two plain questions (or the full IPTC list) and the term they record on the
// parent ingredient of a source without Content Credentials.

import type { AssetKind } from "../api";
import { AFTER, chosenCode, type OfferedOrigin, originsFor, type SourceChoice, TERMS, termOf } from "../sourcetype";
import { esc, fullStop } from "../util";

/** What the questions call a file of `kind`. */
function noun(kind: AssetKind): string {
  return kind === "text" ? "document" : kind;
}

/** The fieldset asking how the file was made, with the term the answers record under it. */
export function sourceFieldset(choice: SourceChoice, kind: AssetKind): string {
  const listed = choice.term !== null;
  return `
      <div class="fieldset source">
        <div class="legend">How was this ${noun(kind)} made? <span class="hint mono">digitalSourceType</span></div>
        ${listed ? termList(choice.term ?? "") : questions(choice, kind)}
        ${recorded(chosenCode(kind, choice), listed)}
      </div>`;
}

/** The two questions; the second only once the first is answered with an origin that asks it. */
function questions(choice: SourceChoice, kind: AssetKind): string {
  const origins = originsFor(kind);
  const current = origins.find((o) => o.id === choice.origin);
  const rows = [radio("source_origin", "", "Don't say", "", !current), ...origins.map((o) => originRow(o, choice))].join("");
  const after = current?.asksAfter ? afterQuestion(choice.after, kind) : "";
  return `
        <div class="choices"><div class="question">Where does it come from?</div>${rows}</div>
        ${after}`;
}

/** One origin, with its variants under it while it is chosen and has more than one. */
function originRow(origin: OfferedOrigin, choice: SourceChoice): string {
  const on = origin.id === choice.origin;
  const row = radio("source_origin", origin.id, origin.label, origin.hint, on);
  if (!on || origin.variants.length < 2) return row;
  const picked = origin.variants.find((v) => v.id === choice.variant) ?? origin.variants[0];
  const variants = origin.variants.map((v) => radio("source_variant", v.id, v.label, "", v === picked)).join("");
  return `${row}<div class="variants">${variants}</div>`;
}

/** The second question, with examples for files of `kind`. */
function afterQuestion(after: string, kind: AssetKind): string {
  const rows = AFTER.map((a) => radio("source_after", a.id, a.label, a.hints[kind] ?? "", a.id === after)).join("");
  return `<div class="choices"><div class="question">What happened to it afterwards?</div>${rows}</div>`;
}

/** A radio button with its label and, muted after it, examples. */
function radio(name: string, value: string, label: string, hint: string, checked: boolean): string {
  const examples = hint ? ` <span class="hint">· ${esc(hint)}</span>` : "";
  return `<label class="radio"><input type="radio" name="${name}" value="${esc(value)}" ${checked ? "checked" : ""} /><span>${esc(label)}${examples}</span></label>`;
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
    return `<p class="recorded"><span class="definition">Nothing is recorded: the C2PA specification lets an ingredient leave it out.</span>${toggle}</p>`;
  }
  const ai = t.ai ? `<span class="ai">Content Credentials viewers show such files as made with generative AI.</span>` : "";
  return `
        <p class="recorded">
          <span>Recorded as <span class="term">${esc(t.code)}</span></span>
          <span class="definition">${esc(t.name)}. ${esc(t.definition)}${fullStop(t.definition)}</span>
          ${ai}${toggle}
        </p>`;
}
