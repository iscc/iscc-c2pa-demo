// Tests of the digital source type questions: run with `pnpm test` (node --test, which strips the types).

import assert from "node:assert/strict";
import { test } from "node:test";
import type { AssetKind } from "./api";
import { AFTER, chosenCode, guidedCode, newSourceChoice, originsFor, TERMS, termOf, termUri, toggleList } from "./sourcetype.ts";

const KINDS: AssetKind[] = ["image", "text", "audio", "video"];

/** The code the answers stand for, for an image unless `kind` says otherwise. */
function code(origin: string, after = "nothing", variant = "", kind: AssetKind = "image"): string | null {
  return guidedCode(kind, { origin, variant, after, term: null });
}

/** Every code some combination of answers leads to, over all kinds. */
function reachable(): Set<string> {
  const codes = new Set<string>();
  for (const kind of KINDS) {
    for (const origin of originsFor(kind)) {
      for (const variant of origin.variants) {
        for (const after of AFTER) codes.add(code(origin.id, after.id, variant.id, kind) ?? "");
      }
    }
  }
  return codes;
}

test("the list holds the 17 active IPTC terms, each once", () => {
  const codes = TERMS.map((t) => t.code);
  assert.equal(codes.length, 17);
  assert.equal(new Set(codes).size, 17);
  for (const retired of ["minorHumanEdits", "digitalArt", "softwareImage"]) assert.equal(termOf(retired), undefined);
});

test("the generative AI terms are the three of the C2PA AI labelling guidance", () => {
  const ai = TERMS.filter((t) => t.ai).map((t) => t.code);
  assert.deepEqual(ai.sort(), ["compositeSynthetic", "compositeWithTrainedAlgorithmicMedia", "trainedAlgorithmicMedia"]);
});

test("every answer leads to a term on the list", () => {
  for (const c of reachable()) assert.ok(termOf(c), `${c} is not an active IPTC term`);
});

test("only the rare terms need the full list", () => {
  const missing = TERMS.map((t) => t.code).filter((c) => !reachable().has(c));
  assert.deepEqual(missing.sort(), ["computationalCapture", "virtualRecording"]);
});

test("a capture becomes human-edited, altered or edited with AI by what followed", () => {
  assert.equal(code("captured"), "digitalCapture");
  assert.equal(code("captured", "hand"), "humanEdits");
  assert.equal(code("captured", "auto"), "algorithmicallyEnhanced");
  assert.equal(code("captured", "genai"), "compositeWithTrainedAlgorithmicMedia");
  assert.equal(code("screen", "hand"), "humanEdits");
  assert.equal(code("software", "auto", "data"), "algorithmicallyEnhanced");
});

test("an AI generation stays generative AI whatever followed", () => {
  for (const after of AFTER) assert.equal(code("ai", after.id), "trainedAlgorithmicMedia");
});

test("a person's work stays a digital creation unless generative AI changed it", () => {
  assert.equal(code("person", "nothing"), "digitalCreation");
  assert.equal(code("person", "hand"), "digitalCreation");
  assert.equal(code("person", "auto"), "digitalCreation");
  assert.equal(code("person", "genai"), "compositeWithTrainedAlgorithmicMedia");
});

test("a composite keeps its term unless generative AI changed it", () => {
  assert.equal(code("combined", "hand", "captures"), "compositeCapture");
  assert.equal(code("combined", "auto", "with-ai"), "compositeSynthetic");
  assert.equal(code("combined", "nothing", "other"), "composite");
  assert.equal(code("combined", "genai", "captures"), "compositeWithTrainedAlgorithmicMedia");
});

test("variants pick the scan's medium and the software's input", () => {
  assert.equal(code("scanned"), "print");
  assert.equal(code("scanned", "nothing", "negative"), "negativeFilm");
  assert.equal(code("scanned", "nothing", "slide"), "positiveFilm");
  assert.equal(code("software", "nothing", "formula"), "algorithmicMedia");
  assert.equal(code("software", "nothing", "data"), "dataDrivenMedia");
  assert.equal(code("scanned", "nothing", "unknown"), "print");
});

test("each kind is offered only what fits it", () => {
  const ids = (kind: AssetKind) => originsFor(kind).map((o) => o.id);
  assert.deepEqual(ids("image"), ["captured", "scanned", "screen", "person", "ai", "software", "combined"]);
  assert.deepEqual(ids("text"), ["scanned", "person", "ai", "software", "combined"]);
  assert.deepEqual(ids("audio"), ["captured", "person", "ai", "software", "combined"]);
  assert.deepEqual(ids("video"), ["captured", "screen", "person", "ai", "software", "combined"]);
  const scanned = (kind: AssetKind) =>
    originsFor(kind)
      .find((o) => o.id === "scanned")
      ?.variants.map((v) => v.code);
  assert.deepEqual(scanned("text"), ["print"]);
  const combined = originsFor("text")
    .find((o) => o.id === "combined")
    ?.variants.map((v) => v.code);
  assert.deepEqual(combined, ["composite", "compositeSynthetic"]);
});

test("no default answer claims generative AI the signer did not pick", () => {
  for (const kind of KINDS) {
    for (const origin of originsFor(kind).filter((o) => o.id !== "ai")) {
      const term = termOf(code(origin.id, "nothing", "", kind) ?? "");
      assert.equal(term?.ai, false, `${kind} ${origin.id} defaults to ${term?.code}`);
    }
  }
  assert.equal(code("combined", "nothing", "", "text"), "composite");
  assert.equal(code("combined"), "composite");
});

test("only an AI generation skips the second question", () => {
  const skipping = originsFor("image").filter((o) => !o.asksAfter);
  assert.deepEqual(
    skipping.map((o) => o.id),
    ["ai"],
  );
});

test("saying nothing, or an origin the kind is not offered, records no term", () => {
  assert.equal(chosenCode("image", newSourceChoice()), null);
  assert.equal(code("captured", "nothing", "", "text"), null);
  assert.equal(code("unknown"), null);
});

test("the full list starts at the answered term and gives the questions back", () => {
  const answered = { origin: "captured", variant: "", after: "genai", term: null };
  const listed = toggleList("image", answered);
  assert.equal(listed.term, "compositeWithTrainedAlgorithmicMedia");
  assert.equal(chosenCode("image", { ...listed, term: "virtualRecording" }), "virtualRecording");
  assert.deepEqual(toggleList("image", listed), answered);
  assert.equal(toggleList("image", newSourceChoice()).term, "");
});

test("a listed choice of none, or of a term not on the list, records no term", () => {
  assert.equal(chosenCode("image", { ...newSourceChoice(), origin: "ai", term: "" }), null);
  assert.equal(chosenCode("image", { ...newSourceChoice(), term: "digitalArt" }), null);
});

test("terms are recorded as IPTC URIs", () => {
  assert.equal(termUri("digitalCapture"), "http://cv.iptc.org/newscodes/digitalsourcetype/digitalCapture");
});
