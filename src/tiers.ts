// The kind of match between two files, from the similarity of their ISCC-UNITs: the tiers of the Tiered Semantic
// Ranking in iscc-hub (rerank.py), as the ISCC Foundation's explainer "Understanding ISCC ranking" (October 2026)
// shows them. A tier names which units agree and how strongly; it labels computed scores and says nothing about how
// the two files came about. No DOM, no value imports (node --test runs it).

/** A unit counts as high at this similarity. */
export const HIGH = 0.85;
/** A unit agrees at this similarity. */
export const SOFT = 0.7;
/** A unit backs another up at this similarity. */
export const WEAK = 0.6;

/** The units a tier looks at. */
export type ScoredUnit = "meta" | "semantic" | "content" | "data" | "instance";

/** Similarity of each unit, 0 to 1; a unit that cannot be compared is left out, and the Instance-Code is 1 when
 * equal and left out otherwise (an exact hash has no degree of similarity). */
export type Scores = Partial<Record<ScoredUnit, number>>;

/** A kind of match, from the strongest to the weakest, then `incoherent`, whose units contradict each other. */
export type Tier = "exact" | "near" | "reencoded" | "adaptation" | "corroborated" | "title" | "isolated" | "incoherent";

/** Each tier's name and the rule that gives it, as a phrase. */
export const TIERS: Record<Tier, { name: string; rule: string }> = {
  exact: { name: "Exact", rule: "the Instance-Code is equal" },
  near: { name: "Near-identical", rule: "Data-Code and Content-Code at 85% or more" },
  reencoded: { name: "Re-encoded", rule: "Content-Code at 85% or more, and another unit at 70% or more" },
  adaptation: { name: "Adaptation", rule: "Semantic-Code at 85% or more, and another unit at 70% or more" },
  corroborated: { name: "Corroborated", rule: "one unit at 70% or more, and another at 60% or more" },
  title: { name: "Title only", rule: "only the Meta-Code, at 85% or more" },
  isolated: { name: "Isolated", rule: "one unit at 70% or more, and no other at 60%" },
  incoherent: { name: "Incoherent", rule: "the Instance-Code is equal, but the Data-Code or Content-Code is below 70%" },
};

/** What a match without a tier is called, and why it has none. */
export const UNRANKED = { name: "No reliable match", rule: "no unit at 70% or more" };

/** Whether `score` is present and at least `floor`. */
function atLeast(score: number | undefined, floor: number): boolean {
  return score !== undefined && score >= floor;
}

/** The tier of a match with an equal Instance-Code: exact, unless the Data-Code or Content-Code is below 70%. */
function exactTier(s: Scores): Tier {
  const below = (v: number | undefined) => v !== undefined && v < SOFT;
  return below(s.data) || below(s.content) ? "incoherent" : "exact";
}

/** The tier of a match with `s`, or null when no unit agrees: the first rule that holds, in iscc-hub's order. A lone
 * high Semantic-Code stays isolated (covers.iscc.io lets it count as an adaptation). */
export function tier(s: Scores): Tier | null {
  if (atLeast(s.instance, HIGH)) return exactTier(s);
  const values = Object.values(s);
  const soft = values.filter((v) => v >= SOFT).length;
  const weak = values.filter((v) => v >= WEAK).length;
  if (atLeast(s.data, HIGH) && (s.content === undefined || s.content >= HIGH)) return "near";
  if (atLeast(s.content, HIGH) && soft >= 2) return "reencoded";
  if (atLeast(s.semantic, HIGH) && soft >= 2) return "adaptation";
  if (soft >= 1 && weak >= 2) return "corroborated";
  if (atLeast(s.meta, HIGH) && weak === 1) return "title";
  return soft === 1 ? "isolated" : null;
}

/** The position of `t` in a list sorted by kind of match: the strongest first, matches without a tier last. */
export function tierRank(t: Tier | null): number {
  return t === null ? Object.keys(TIERS).length : Object.keys(TIERS).indexOf(t);
}
