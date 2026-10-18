// Names for files that reach the page without a path: a picture pasted from the clipboard comes as "image.png"
// every time, so it is named after the moment it was pasted. No DOM, no value imports (node --test runs it).

const pad = (n: number) => String(n).padStart(2, "0");

/** `pasted-YYYY-MM-DD-HHMMSS.<ext>` in local time, the extension taken from `name` (lowercase), else png. */
export function pastedName(name: string, at: Date): string {
  const dot = name.lastIndexOf(".");
  const ext = dot > 0 && dot < name.length - 1 ? name.slice(dot + 1).toLowerCase() : "png";
  const day = `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}`;
  const time = `${pad(at.getHours())}${pad(at.getMinutes())}${pad(at.getSeconds())}`;
  return `pasted-${day}-${time}.${ext}`;
}
