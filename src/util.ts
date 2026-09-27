// Small rendering helpers shared by the views.

/** Escape text for insertion into HTML. */
export function esc(value: unknown): string {
  return String(value ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/** Human readable byte count. */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB"];
  let value = bytes / 1024;
  let i = 0;
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024;
    i++;
  }
  return `${value.toFixed(value >= 100 ? 0 : 1)} ${units[i]}`;
}

/** Duration as m:ss, or h:mm:ss from one hour. */
export function formatDuration(seconds: number): string {
  const total = Math.round(seconds);
  const [h, m, s] = [Math.floor(total / 3600), Math.floor((total % 3600) / 60), total % 60];
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

/** Join words as "a", "a and b", "a, b and c". */
export function listJoin(words: string[]): string {
  return words.length < 2 ? (words[0] ?? "") : `${words.slice(0, -1).join(", ")} and ${words[words.length - 1]}`;
}

/** A timestamp in local time with its UTC offset; the raw value when it cannot be parsed. */
export function formatTime(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return new Intl.DateTimeFormat(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    timeZoneName: "shortOffset",
  }).format(date);
}

/** Full stop after `text`, unless it already ends with one ("Inc."). */
export function fullStop(text: string): string {
  return text.endsWith(".") ? "" : ".";
}

/** Render an ISCC string with a muted prefix. */
export function isccHtml(iscc: string): string {
  const [prefix, body] = iscc.includes(":") ? iscc.split(/:(.*)/s) : ["", iscc];
  return prefix ? `<span class="prefix">${esc(prefix)}:</span>${esc(body)}` : esc(body);
}

/** Percentage label for a similarity fraction. */
export function percent(value: number): string {
  return `${Math.round(value * 1000) / 10}%`;
}

/** Copy text to the clipboard; the web view may refuse, in which case nothing happens. */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/** Split a path into directory, stem and extension. */
export function splitPath(path: string): { dir: string; stem: string; ext: string; sep: string } {
  const sep = path.includes("\\") ? "\\" : "/";
  const idx = path.lastIndexOf(sep);
  const dir = idx >= 0 ? path.slice(0, idx) : "";
  const file = idx >= 0 ? path.slice(idx + 1) : path;
  const dot = file.lastIndexOf(".");
  return { dir, stem: dot > 0 ? file.slice(0, dot) : file, ext: dot > 0 ? file.slice(dot) : "", sep };
}

/** Pretty JSON for the raw views. */
export function json(value: unknown): string {
  return JSON.stringify(value, null, 2);
}

/** Last path segment of a JUMBF or file URI, for compact display. */
export function shortUri(uri: string | undefined): string {
  if (!uri) return "";
  const parts = uri.split("/");
  return parts[parts.length - 1] || uri;
}
