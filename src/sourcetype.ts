// How a file was made, as the Sign tab asks it: the IPTC digital source types, and two plain questions (where the
// content comes from, what happened to it afterwards) that the app turns into one of those terms for the creator.
// No DOM and no value imports, so `node --test` runs it as it is.

import type { AssetKind } from "./api";

/** Namespace of the IPTC NewsCodes digital source types. */
const IPTC = "http://cv.iptc.org/newscodes/digitalsourcetype/";

/** One IPTC term: its code, IPTC's name and definition (en-GB), the group the full list shows it in, and whether
 * the C2PA's AI labelling guidance counts it as generative AI content. */
export interface Term {
  code: string;
  name: string;
  definition: string;
  group: string;
  ai: boolean;
}

/** Every active IPTC digital source type, in groups. Left out: the retired minorHumanEdits, digitalArt and
 * softwareImage, and the C2PA's own empty and trainedAlgorithmicData, which describe a blank start and data
 * formats, never an existing media file. */
export const TERMS: Term[] = [
  term(
    "digitalCapture",
    "Digital capture sampled from real life",
    "The media was captured from a real-life source using a digital camera or digital recording device",
    "Captured from real life",
  ),
  term(
    "computationalCapture",
    "Multi-frame computational capture sampled from real life",
    "The media is the result of capturing multiple frames from a real-life source using a digital camera or digital recording device, then automatically merging them into a single frame using digital signal processing techniques and/or non-generative AI. Includes High Dynamic Range (HDR) processing common in smartphone camera apps.",
    "Captured from real life",
  ),
  term(
    "negativeFilm",
    "Digitised from a transparent negative",
    "The media was digitised from a negative on film or other transparent medium",
    "Captured from real life",
  ),
  term(
    "positiveFilm",
    "Digitised from a transparent positive",
    "The media was digitised from a positive on a transparency or other transparent medium",
    "Captured from real life",
  ),
  term(
    "print",
    "Digitised from a non-transparent medium",
    "The media was digitised from a non-transparent medium such as a photographic print",
    "Captured from real life",
  ),
  term(
    "screenCapture",
    "Screen capture",
    "A capture of the contents of the screen of a computer or mobile device",
    "Captured from real life",
  ),
  term(
    "digitalCreation",
    "Digital creation",
    "Media created by a human using non-generative tools",
    "Made or edited without generative AI",
  ),
  term(
    "humanEdits",
    "Human-edited media",
    "Augmentation, correction or enhancement by one or more humans using non-generative tools",
    "Made or edited without generative AI",
  ),
  term(
    "algorithmicallyEnhanced",
    "Algorithmically-altered media",
    "Modification or correction by algorithm without changing the main content of the media, initiated or configured by a human, such as sharpening or applying noise reduction",
    "Made or edited without generative AI",
  ),
  term(
    "algorithmicMedia",
    "Pure algorithmic media",
    "Media created purely by an algorithm not based on any sampled training data, e.g. an image created by software using a mathematical formula",
    "Made or edited without generative AI",
  ),
  term(
    "dataDrivenMedia",
    "Data-driven media",
    "Digital media representation of data via human programming or creativity",
    "Made or edited without generative AI",
  ),
  term(
    "trainedAlgorithmicMedia",
    "Created using Generative AI",
    "Digital media created algorithmically using an Artificial Intelligence model trained on captured content",
    "Generative AI",
    true,
  ),
  term(
    "compositeWithTrainedAlgorithmicMedia",
    "Edited using Generative AI",
    "Augmentation, correction or enhancement using a Generative AI model, such as with inpainting or outpainting operations",
    "Generative AI",
    true,
  ),
  term(
    "compositeSynthetic",
    "Composite including generative AI elements",
    "Mix or composite of several elements, at least one of which is Generative AI",
    "Generative AI",
    true,
  ),
  term(
    "compositeCapture",
    "Composite of captured elements",
    "Mix or composite of several elements that are all captures of real life",
    "Mixed",
  ),
  term("composite", "Composite of elements", "Mix or composite of several elements, any of which may or may not be generative AI", "Mixed"),
  term(
    "virtualRecording",
    "Virtual event recording",
    "Live recording of virtual event based on Generative AI and/or captured elements",
    "Mixed",
  ),
];

/** A `Term`; `ai` defaults to false. */
function term(code: string, name: string, definition: string, group: string, ai = false): Term {
  return { code, name, definition, group, ai };
}

/** The term with this code, if it is in `TERMS`. */
export function termOf(code: string): Term | undefined {
  return TERMS.find((t) => t.code === code);
}

/** The full URI of a term code, as the manifest records it. */
export function termUri(code: string): string {
  return IPTC + code;
}

/** How the answer to the second question changes the term of an origin: `capture` becomes human-edited or
 * algorithmically altered, `creation` stays a digital creation (its tools are the person's), `kept` stays as it is
 * (a composite is edited by nature), and `ai` stays generative AI whatever followed, so hand edits never hide it. */
type Edits = "capture" | "creation" | "kept" | "ai";

/** A more precise answer to the first question, mapped to its term; offered for the listed kinds only, all when
 * `kinds` is missing. */
export interface Variant {
  id: string;
  label: string;
  code: string;
  kinds?: AssetKind[];
}

/** An answer to "where does it come from?": its label and examples per kind (a kind without a label is not offered
 * it), its variants (the first is the default, so it never claims generative AI the signer did not pick) and how
 * later edits change its term. */
interface Origin {
  id: string;
  labels: Partial<Record<AssetKind, string>>;
  hints: Partial<Record<AssetKind, string>>;
  variants: Variant[];
  edits: Edits;
}

const ALL_KINDS: AssetKind[] = ["image", "text", "audio", "video"];

/** The same text for every kind. */
function every(text: string): Record<AssetKind, string> {
  return Object.fromEntries(ALL_KINDS.map((k) => [k, text])) as Record<AssetKind, string>;
}

const ORIGINS: Origin[] = [
  {
    id: "captured",
    labels: { image: "Photographed", video: "Filmed", audio: "Recorded" },
    hints: { image: "camera or phone", video: "camera or phone", audio: "microphone or recorder" },
    variants: [{ id: "capture", label: "", code: "digitalCapture" }],
    edits: "capture",
  },
  {
    id: "scanned",
    labels: { image: "Scanned or digitised", text: "Scanned from paper" },
    hints: {},
    variants: [
      { id: "print", label: "from a print or paper", code: "print" },
      { id: "negative", label: "from a film negative", code: "negativeFilm", kinds: ["image"] },
      { id: "slide", label: "from a slide or other transparency", code: "positiveFilm", kinds: ["image"] },
    ],
    edits: "capture",
  },
  {
    id: "screen",
    labels: { image: "Screenshot", video: "Screen recording" },
    hints: {},
    variants: [{ id: "screen", label: "", code: "screenCapture" }],
    edits: "capture",
  },
  {
    id: "person",
    labels: {
      image: "Made by a person on a computer",
      text: "Written by a person",
      audio: "Composed or produced on a computer",
      video: "Animated or made on a computer",
    },
    hints: {
      image: "drawn, painted, designed",
      audio: "software instruments, sound design",
      video: "animation, motion graphics",
    },
    variants: [{ id: "creation", label: "", code: "digitalCreation" }],
    edits: "creation",
  },
  {
    id: "ai",
    labels: every("Generated by AI from a prompt"),
    hints: every("also when edited afterwards"),
    variants: [{ id: "generated", label: "", code: "trainedAlgorithmicMedia" }],
    edits: "ai",
  },
  {
    id: "software",
    labels: every("Generated by software without AI"),
    hints: {},
    variants: [
      { id: "formula", label: "from a formula or a program", code: "algorithmicMedia" },
      { id: "data", label: "from data, such as a chart or an automated report", code: "dataDrivenMedia" },
    ],
    edits: "capture",
  },
  {
    id: "combined",
    labels: every("Put together from several pieces"),
    hints: {
      image: "collage, montage",
      text: "compiled from several texts",
      audio: "mix of recordings or samples",
      video: "edited together from clips",
    },
    variants: [
      { id: "other", label: "mixed or unknown pieces", code: "composite" },
      {
        id: "captures",
        label: "all of them photos or recordings of real life",
        code: "compositeCapture",
        kinds: ["image", "audio", "video"],
      },
      { id: "with-ai", label: "at least one of them generated by AI", code: "compositeSynthetic" },
    ],
    edits: "kept",
  },
];

/** An answer to "what happened to it afterwards?", with examples per kind. */
interface After {
  id: string;
  label: string;
  hints: Partial<Record<AssetKind, string>>;
}

export const AFTER: After[] = [
  { id: "nothing", label: "Nothing", hints: every("converted or resized at most") },
  {
    id: "hand",
    label: "Edited by hand, without generative AI",
    hints: { image: "retouched, cropped, colours", text: "revised, corrected", audio: "cut, mixed, mastered", video: "cut, colour graded" },
  },
  {
    id: "auto",
    label: "Enhanced automatically, without adding content",
    hints: {
      image: "noise reduction, sharpening",
      text: "spelling corrected",
      audio: "noise reduction, levelling",
      video: "stabilised, noise reduced",
    },
  },
  {
    id: "genai",
    label: "Changed with generative AI",
    hints: {
      image: "generative fill, expand, AI retouching",
      text: "passages written or rewritten by AI",
      audio: "AI voices or AI-generated parts",
      video: "AI-generated or replaced scenes",
    },
  },
];

/** The creator's answers: origin ("" says nothing), variant ("" or an unknown id means the first) and what happened
 * afterwards; `term` is the code picked from the full list ("" for none), null while the questions are used. */
export interface SourceChoice {
  origin: string;
  variant: string;
  after: string;
  term: string | null;
}

/** Answers that record nothing yet. */
export function newSourceChoice(): SourceChoice {
  return { origin: "", variant: "", after: "nothing", term: null };
}

/** An origin as offered for a kind: its label and hint for that kind and the variants that apply to it. */
export interface OfferedOrigin {
  id: string;
  label: string;
  hint: string;
  variants: Variant[];
  /** Whether the second question applies; generative AI stays generative AI whatever followed. */
  asksAfter: boolean;
}

/** The origins offered for files of `kind`, in display order. */
export function originsFor(kind: AssetKind): OfferedOrigin[] {
  return ORIGINS.filter((o) => o.labels[kind]).map((o) => ({
    id: o.id,
    label: o.labels[kind] ?? "",
    hint: o.hints[kind] ?? "",
    variants: o.variants.filter((v) => !v.kinds || v.kinds.includes(kind)),
    asksAfter: o.edits !== "ai",
  }));
}

/** The term code the answers stand for; null when they say nothing (or name an origin `kind` is not offered). */
export function guidedCode(kind: AssetKind, choice: SourceChoice): string | null {
  const origin = ORIGINS.find((o) => o.id === choice.origin && o.labels[kind]);
  if (!origin) return null;
  const variants = origin.variants.filter((v) => !v.kinds || v.kinds.includes(kind));
  const base = (variants.find((v) => v.id === choice.variant) ?? variants[0]).code;
  return edited(origin.edits, base, choice.after);
}

/** The term of `base` after what happened to it (an `AFTER` id). */
function edited(edits: Edits, base: string, after: string): string {
  if (edits === "ai") return base;
  if (after === "genai") return "compositeWithTrainedAlgorithmicMedia";
  if (edits !== "capture") return base;
  if (after === "hand") return "humanEdits";
  if (after === "auto") return "algorithmicallyEnhanced";
  return base;
}

/** The term code the choice records: the one picked from the full list, else the one the answers stand for; null
 * records none. */
export function chosenCode(kind: AssetKind, choice: SourceChoice): string | null {
  if (choice.term !== null) return termOf(choice.term) ? choice.term : null;
  return guidedCode(kind, choice);
}

/** Switch between the questions and the full list; the list starts at the term the answers stand for. */
export function toggleList(kind: AssetKind, choice: SourceChoice): SourceChoice {
  if (choice.term !== null) return { ...choice, term: null };
  return { ...choice, term: guidedCode(kind, choice) ?? "" };
}
