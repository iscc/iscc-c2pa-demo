// Tests of the names of pasted files: run with `pnpm test`.

import assert from "node:assert/strict";
import { test } from "node:test";
import { pastedName } from "./dropped.ts";

const at = new Date(2026, 9, 18, 9, 5, 7);

test("names a pasted picture after the moment it was pasted", () => {
  assert.equal(pastedName("image.png", at), "pasted-2026-10-18-090507.png");
});

test("keeps the pasted file's extension in lowercase", () => {
  assert.equal(pastedName("Photo.JPG", at), "pasted-2026-10-18-090507.jpg");
  assert.equal(pastedName("a.b.webp", at), "pasted-2026-10-18-090507.webp");
});

test("falls back to png without an extension", () => {
  for (const name of ["", "image", ".png", "image."]) assert.equal(pastedName(name, at), "pasted-2026-10-18-090507.png");
});
