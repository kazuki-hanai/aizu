#!/usr/bin/env node

import { createHash } from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";

const ROOT = resolve(import.meta.dirname, "..");
const CHECKSUMS = "assets/branding/agents/SHA256SUMS";

function sha256(path) {
  return createHash("sha256").update(readFileSync(resolve(ROOT, path))).digest("hex");
}

const entries = readFileSync(resolve(ROOT, CHECKSUMS), "utf8")
  .trim()
  .split("\n")
  .map((line) => {
    const match = line.match(/^([0-9a-f]{64}) {2}(.+)$/u);
    if (!match) throw new Error(`${CHECKSUMS}: malformed checksum line`);
    return { expected: match[1], path: match[2] };
  });

const listed = new Set(entries.map(({ path }) => path));
const vendorFiles = ["anthropic", "openai"].flatMap((vendor) =>
  readdirSync(resolve(ROOT, `assets/branding/agents/${vendor}`), { withFileTypes: true })
    .filter((entry) => entry.isFile())
    .map((entry) => `assets/branding/agents/${vendor}/${entry.name}`),
);

if (listed.size !== entries.length) throw new Error(`${CHECKSUMS}: duplicate path`);
if (listed.size !== vendorFiles.length || vendorFiles.some((path) => !listed.has(path))) {
  throw new Error(`${CHECKSUMS}: vendor asset inventory mismatch`);
}

for (const { expected, path } of entries) {
  const actual = sha256(path);
  if (actual !== expected) throw new Error(`${path}: vendor checksum mismatch`);
}

console.log(`validated ${entries.length} official agent assets`);
