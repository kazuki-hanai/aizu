#!/usr/bin/env node

import { createHash } from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import { relative, resolve } from "node:path";

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
function inventory(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) return inventory(path);
    if (!entry.isFile()) throw new Error(`${relative(ROOT, path)}: unsupported vendor entry`);
    return [relative(ROOT, path)];
  });
}

const vendorFiles = ["anthropic", "openai"].flatMap((vendor) =>
  inventory(resolve(ROOT, `assets/branding/agents/${vendor}`)),
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
