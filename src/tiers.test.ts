// Tests of the kind of match: run with `pnpm test`. The first test takes the 256-bit scores and tiers of the nine
// example files in the ISCC Foundation's explainer "Understanding ISCC ranking", computed there by a re-implementation
// of iscc-hub's rerank.py.

import assert from "node:assert/strict";
import { test } from "node:test";
import { type Scores, type Tier, tier, tierRank } from "./tiers.ts";

test("gives the tiers of the explainer's example files", () => {
  const cases: [string, Scores, Tier | null][] = [
    ["exact copy", { meta: 1, semantic: 1, content: 1, data: 1, instance: 1 }, "exact"],
    ["same file, new title", { meta: 0.598, semantic: 1, content: 1, data: 1, instance: 1 }, "exact"],
    ["tagged copy", { meta: 1, semantic: 1, content: 1, data: 1 }, "near"],
    ["re-saved as JPEG", { meta: 1, semantic: 1, content: 1, data: 0.473 }, "reencoded"],
    ["cropped", { meta: 1, semantic: 0.91, content: 0.656, data: 0.496 }, "adaptation"],
    ["mirrored", { meta: 1, semantic: 0.926, content: 0.508, data: 0.519 }, "adaptation"],
    ["same idea, new picture", { meta: 1, semantic: 0.766, content: 0.688, data: 0.512 }, "corroborated"],
    ["same title, other picture", { meta: 1, semantic: 0.57, content: 0.578, data: 0.508 }, "title"],
    ["unrelated", { meta: 0.508, semantic: 0.488, content: 0.57, data: 0.519 }, null],
  ];
  for (const [file, scores, expected] of cases) assert.equal(tier(scores), expected, file);
});

test("calls an equal Instance-Code incoherent when the Data-Code or Content-Code disagrees", () => {
  assert.equal(tier({ instance: 1, data: 0.42, content: 1 }), "incoherent");
  assert.equal(tier({ instance: 1, data: 1, content: 0.5 }), "incoherent");
  assert.equal(tier({ instance: 1 }), "exact");
});

test("needs a second unit for a high Content-Code or Semantic-Code", () => {
  assert.equal(tier({ content: 0.99, data: 0.45, meta: 0.5 }), "isolated");
  assert.equal(tier({ content: 0.99, data: 0.45, meta: 0.72 }), "reencoded");
  assert.equal(tier({ semantic: 0.96, content: 0.82, data: 0.42, meta: 0.51 }), "adaptation");
  assert.equal(tier({ content: 0.82, data: 0.42, meta: 0.51 }), "isolated");
});

test("takes a missing Content-Code as no objection to a near-identical Data-Code", () => {
  assert.equal(tier({ data: 0.9, meta: 0.3 }), "near");
  assert.equal(tier({ data: 0.9, content: 0.8 }), "corroborated");
});

test("counts the thresholds as reached at their exact value", () => {
  assert.equal(tier({ content: 0.85, data: 0.7 }), "reencoded");
  assert.equal(tier({ content: 0.7, data: 0.6 }), "corroborated");
  assert.equal(tier({ content: 0.69, data: 0.6 }), null);
});

test("sorts the strongest kind first and matches without a tier last", () => {
  const sorted = (["isolated", null, "exact", "adaptation", "incoherent"] as (Tier | null)[]).sort((a, b) => tierRank(a) - tierRank(b));
  assert.deepEqual(sorted, ["exact", "adaptation", "isolated", "incoherent", null]);
});
