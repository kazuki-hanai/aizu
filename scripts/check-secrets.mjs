#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

const files = execFileSync(
  "git",
  ["ls-files", "--cached", "--others", "--exclude-standard", "-z"],
  { encoding: "utf8" },
)
  .split("\0")
  .filter(Boolean);
const patterns = [
  /-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----/,
  /\bgh[opusr]_[A-Za-z0-9_]{36,}\b/,
  /\bsk-(?:proj-)?[A-Za-z0-9_-]{24,}\b/,
  /\bAKIA[0-9A-Z]{16}\b/,
];
const findings = [];

for (const file of files) {
  let content;
  try {
    content = readFileSync(file, "utf8");
  } catch {
    continue;
  }
  for (const pattern of patterns) {
    if (pattern.test(content)) findings.push(`${file}: ${pattern.source}`);
  }
}

if (findings.length > 0) {
  console.error(`credential-like material found:\n${findings.join("\n")}`);
  process.exit(1);
}
console.log(`checked ${files.length} repository files for credential material`);
