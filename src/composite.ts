// The compact ISCC-CODE (MainType ISCC, 64 bits of each unit) composed from ISCC-UNITs, as iscc-core's
// gen_iscc_code_v0 does without `wide`. Composed here rather than in the backend, so it always follows the units the
// unit list shows (a Semantic-Code switched off leaves the open inspection at once). No DOM, no value imports
// (node --test runs it).

const ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/** MainTypes of the units an ISCC-CODE takes, and of the code itself. */
const META = 0;
const SEMANTIC = 1;
const CONTENT = 2;
const DATA = 3;
const INSTANCE = 4;
const ISCC = 5;

/** SubTypes of an ISCC-CODE without a Content- or Semantic-Code. */
const SUM = 5;
const NONE = 6;

/** Bytes of RFC 4648 base32 without padding. */
function fromBase32(text: string): number[] {
  const bytes: number[] = [];
  let buffer = 0;
  let bits = 0;
  for (const char of text) {
    const value = ALPHABET.indexOf(char);
    if (value < 0) throw new Error(`not base32: ${char}`);
    buffer = ((buffer << 5) | value) & 0xffff;
    bits += 5;
    if (bits >= 8) {
      bits -= 8;
      bytes.push((buffer >> bits) & 0xff);
    }
  }
  return bytes;
}

/** RFC 4648 base32 of `bytes` without padding. */
function toBase32(bytes: number[]): string {
  let text = "";
  let buffer = 0;
  let bits = 0;
  for (const byte of bytes) {
    buffer = ((buffer << 8) | byte) & 0xffff;
    bits += 8;
    while (bits >= 5) {
      bits -= 5;
      text += ALPHABET[(buffer >> bits) & 31];
    }
  }
  return bits > 0 ? text + ALPHABET[(buffer << (5 - bits)) & 31] : text;
}

/** A unit's MainType, SubType and body; null for anything but a unit of at least 64 bits with a two-byte header
 * (every unit this app computes). */
function decodeUnit(iscc: string): { main: number; sub: number; body: number[] } | null {
  const bytes = fromBase32(iscc.replace(/^ISCC:/, ""));
  const [first, second] = bytes;
  if (first === undefined || second === undefined || (first | second) & 0x88) return null;
  const main = first >> 4;
  if (main > INSTANCE || bytes.length < 10) return null;
  return { main, sub: first & 0x0f, body: bytes.slice(2, 10) };
}

/** SubType of the code: the shared SubType of its Content- and Semantic-Code, else SUM for Data and Instance alone
 * and NONE with a Meta-Code; null when the Content- and Semantic-Code disagree. */
function subtypeOf(units: { main: number; sub: number }[]): number | null {
  const perceptual = units.filter((u) => u.main === SEMANTIC || u.main === CONTENT).map((u) => u.sub);
  if (perceptual.length === 0) return units.length === 2 ? SUM : NONE;
  return perceptual.every((s) => s === perceptual[0]) ? perceptual[0] : null;
}

/** The compact ISCC-CODE of `units` (ISCC strings in any order), or null without a Data- and an Instance-Code, with
 * a unit counted twice, or with units that do not combine. */
export function isccCode(units: string[]): string | null {
  const decoded = units.map(decodeUnit);
  if (decoded.some((u) => u === null)) return null;
  const sorted = (decoded as { main: number; sub: number; body: number[] }[]).sort((a, b) => a.main - b.main);
  const mains = sorted.map((u) => u.main);
  if (new Set(mains).size !== mains.length || !mains.includes(DATA) || !mains.includes(INSTANCE)) return null;
  const sub = subtypeOf(sorted);
  if (sub === null) return null;
  const optional = (mains.includes(CONTENT) ? 1 : 0) | (mains.includes(SEMANTIC) ? 2 : 0) | (mains.includes(META) ? 4 : 0);
  const header = [(ISCC << 4) | sub, optional];
  return `ISCC:${toBase32([...header, ...sorted.flatMap((u) => u.body)])}`;
}
