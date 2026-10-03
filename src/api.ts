// Typed wrappers around the Tauri commands exposed by the Rust core.

import { invoke } from "@tauri-apps/api/core";

export type UnitSlug = "meta" | "semantic" | "content" | "data" | "instance";

export interface IsccUnit {
  unit: UnitSlug;
  name: string;
  iscc: string;
}

export interface UnitMatch {
  embedded: IsccUnit;
  computed: string | null;
  similarity: number | null;
}

/** Informational `bindingMetadata` of a soft-binding assertion (C2PA spec 2.3+). */
export interface BindingMetadata {
  description: string | null;
  contact: string | null;
  informational_url: string | null;
}

export interface SoftBindingSummary {
  alg: string | null;
  supported: boolean;
  value_base64: string;
  units: IsccUnit[];
  matches: UnitMatch[];
  error: string | null;
  metadata: BindingMetadata | null;
  /** Whether the file is source-preserving (IEP-0020); null unless the block decoded as ISCC. */
  preservation: Preservation | null;
}

/** Result of the IEP-0020 Source Preservation check, with the reason when not preserved. */
export type Preservation =
  | "preserved"
  | "changed"
  | "resigned"
  | "no_source_view"
  | "no_instance_code"
  | "signature_invalid"
  | "file_changed";

export interface SignatureSummary {
  alg: string | null;
  issuer: string | null;
  common_name: string | null;
  time: string | null;
  cert_serial_number: string | null;
  timestamp: TimestampSummary;
}

/** Timestamp of the claim signature, judged by the validator's `timeStamp.*` codes. */
export interface TimestampSummary {
  status: "trusted" | "untrusted" | "rejected" | "none";
  /** Trusted because v1 claims skip the trust check of the timestamp service. */
  legacy: boolean;
  time: string | null;
  /** Organisation of the timestamp service's certificate, else its common name. */
  tsa: string | null;
  tsa_detail: string | null;
  /** Explanation of the code that decided the status. */
  reason: string | null;
}

/** The failure that makes a manifest invalid, in plain language. */
export interface InvalidReason {
  /** Plain-language sentence; c2pa's own explanation for codes without one. */
  text: string;
  /** Validation code of that failure; null when c2pa names no failure. */
  code: string | null;
  explanation: string | null;
  /** Further failures that make the manifest invalid. */
  more: number;
}

export interface AssertionSummary {
  label: string;
  data: unknown;
}

export interface IngredientSummary {
  title: string | null;
  relationship: string;
  format: string | null;
  /** How the content of an ingredient without Content Credentials was made (URI). */
  digital_source_type: string | null;
  validation_state: string | null;
}

export interface ValidationStatus {
  code: string;
  url?: string;
  explanation?: string;
}

export interface ValidationResults {
  activeManifest?: {
    success?: ValidationStatus[];
    informational?: ValidationStatus[];
    failure?: ValidationStatus[];
  };
  ingredientDeltas?: unknown[];
  specVersion?: string;
}

export interface ManifestSummary {
  label: string;
  title: string | null;
  claim_generator: string | null;
  manifest_count: number;
  validation_state: "Trusted" | "Valid" | "Invalid" | string;
  /** Why the manifest is invalid; set exactly when `validation_state` is `Invalid`. */
  invalid_reason: InvalidReason | null;
  /** URI of the trust list the active manifest's signer chains to; null when untrusted. */
  trust_list: string | null;
  validation: ValidationResults | null;
  /** True when the file has a source view: the file without the byte ranges the data hash of an embedded manifest excludes, or the file itself for a sidecar manifest. */
  source_view: boolean;
  /** File name of the sidecar the manifest store was read from; null when it is embedded. */
  sidecar: string | null;
  signature: SignatureSummary | null;
  assertions: AssertionSummary[];
  ingredients: IngredientSummary[];
  soft_bindings: SoftBindingSummary[];
  training_mining: TrainingMining | null;
  /** Claim thumbnail of the active manifest as a data URL; empty when it has none. */
  thumbnail: string;
}

export interface TrainingEntry {
  use: "allowed" | "notAllowed" | "constrained" | string;
  constraint_info?: string;
}

export interface TrainingMining {
  entries: Record<string, TrainingEntry>;
}

/** Title, description and ISCC metadata behind the Meta-Code (see `metadata.rs`). */
export interface MetaFields {
  name: string;
  description: string | null;
  /** ISCC metadata embedded in the file (`iscc:meta`); replaces the description in the Meta-Code. */
  meta: string | null;
  /** Where `name` came from: the file's metadata, the C2PA manifest title, or the file name. */
  name_source: "metadata" | "manifest" | "filename";
}

export interface Inspection {
  path: string;
  file_name: string;
  suggested_output: string;
  mime: string;
  /** Display name of the format, such as "Word (DOCX)". */
  format_label: string;
  /** Decides which Content-Code applies and how the asset is shown. */
  kind: AssetKind;
  size_bytes: number;
  /** Pixel size of an image (the rendered size of an SVG); 0 for text and audio assets. */
  width: number;
  height: number;
  /** JPEG data URL (the cover or thumbnail of a document, audio cover art); empty when there is nothing to show. */
  preview: string;
  /** Characters of extracted text; null for images and audio. */
  characters: number | null;
  /** Length of the decoded audio in seconds; null for images and text. */
  duration_secs: number | null;
  /** Creator named in the file's own metadata; display only. */
  creator: string | null;
  iscc: IsccUnit[];
  meta_fields: MetaFields;
  /** Why the Meta-Code could not be computed; `iscc` then lacks it. */
  meta_error: string | null;
  /** Why the Content-Code could not be computed (audio too short, a document without text); `iscc` then lacks it. */
  content_error: string | null;
  /** Why this file cannot be signed (an encrypted PDF); null when it can. */
  sign_block: string | null;
  /** What signing does to this file that its owner may not want (breaking a PDF's digital signature). */
  sign_warning: string | null;
  manifest: ManifestSummary | null;
  manifest_json: unknown;
  manifest_error: string | null;
}

export type AssetKind = "image" | "text" | "audio";

/** Formats of one kind of asset, for the start screen and the file dialog. */
export interface KindInfo {
  kind: AssetKind;
  label: string;
  /** Short format names (JPEG, DOCX, MD, ...), in table order. */
  formats: string[];
  extensions: string[];
}

/** A timestamp service offered in the Sign form. */
export interface TsaPreset {
  name: string;
  url: string;
  /** Whether its timestamps validate against the C2PA TSA trust list. */
  trusted: boolean;
}

export interface AppInfo {
  version: string;
  c2pa_version: string;
  claim_generator: string;
  soft_binding_alg: string;
  extensions: string[];
  kinds: KindInfo[];
  /** The first preset is the default. */
  tsa_presets: TsaPreset[];
  tsa_timeout_secs: number;
}

export type Credentials = { kind: "demo" } | { kind: "custom"; cert_path: string; key_path: string; alg: string };

export interface SignRequest {
  source: string;
  output: string;
  title: string;
  description?: string;
  /** The source's embedded ISCC metadata, passed through so the Meta-Code stays reproducible. */
  meta?: string;
  /** Digital source type URI, recorded on the parent ingredient when the source has no Content Credentials; none when absent. */
  source_type?: string;
  units: string[];
  training: Record<string, TrainingEntry>;
  credentials: Credentials;
  /** Timestamp service (RFC 3161); no timestamp when absent. */
  tsa_url?: string;
}

/** What became of the requested timestamp; the inspection tells whether it is trusted. */
export type TimestampOutcome = { status: "off" } | { status: "added" } | { status: "failed"; url: string; reason: string };

export interface SignResult {
  output: string;
  units: IsccUnit[];
  iscc_seq_base64: string;
  timestamp: TimestampOutcome;
  inspection: Inspection;
}

export const appInfo = () => invoke<AppInfo>("app_info");

export const initialPath = () => invoke<string | null>("initial_path");

export const inspectAsset = (path: string) => invoke<Inspection>("inspect_asset", { path });

export const signAsset = (request: SignRequest) => invoke<SignResult>("sign_asset", { request });

export const metaCode = (title: string, description?: string, meta?: string) =>
  invoke<IsccUnit>("meta_code", { title, description: description || null, meta: meta || null });
