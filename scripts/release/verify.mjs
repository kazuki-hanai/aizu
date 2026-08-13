#!/usr/bin/env node

import { createHash } from "node:crypto";
import { createReadStream, lstatSync, readdirSync, readFileSync } from "node:fs";
import { basename, resolve } from "node:path";

async function sha256(path) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}

async function main() {
  const [directory] = process.argv.slice(2);
  if (!directory) throw new Error("usage: verify.mjs <release-assets-directory>");
  const root = resolve(directory);
  const lines = readFileSync(resolve(root, "SHA256SUMS"), "utf8").trim().split("\n");
  if (lines.length === 0) throw new Error("SHA256SUMS is empty");
  const seen = new Set();
  for (const line of lines) {
    const match = /^([0-9a-f]{64})  ([^/]+)$/u.exec(line);
    if (!match) throw new Error(`invalid checksum line: ${line}`);
    const [, expected, name] = match;
    if (seen.has(name)) throw new Error(`duplicate checksum entry: ${name}`);
    seen.add(name);
    const path = resolve(root, name);
    if (!lstatSync(path).isFile()) throw new Error(`release asset is not a regular file: ${name}`);
    const actual = await sha256(path);
    if (actual !== expected) throw new Error(`checksum mismatch: ${name}`);
  }
  const actualNames = readdirSync(root, { withFileTypes: true })
    .map((entry) => {
      if (!entry.isFile()) throw new Error(`unexpected non-file release entry: ${entry.name}`);
      return entry.name;
    })
    .filter((name) => name !== "SHA256SUMS")
    .sort();
  const expectedNames = [...seen].sort();
  if (actualNames.join("\n") !== expectedNames.join("\n")) {
    throw new Error("release directory does not exactly match SHA256SUMS");
  }
  console.log(`verified ${seen.size} checksums in ${basename(root)}`);
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
