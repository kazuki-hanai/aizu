#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { appendFileSync } from "node:fs";

const DOC_ONLY = /^(?:AGENTS\.md|README\.md|docs\/.*\.md)$/u;
const PACKAGING_INPUT = /^(?:Cargo\.(?:toml|lock)|assets\/(?:audio|branding)\/|apps\/desktop\/src-tauri\/(?:Cargo\.toml|build\.rs|icons\/|resources\/|tauri\.conf\.json)|scripts\/(?:build-dev-dmg|install-dev-app|prepare-desktop-cli)\.sh|scripts\/ci\/verify-dev-dmg\.sh)/u;

export function classifyChanges(paths) {
  const files = paths.filter(Boolean);
  return {
    macosRequired: files.some((path) => !DOC_ONLY.test(path)),
    packageRequired: files.some((path) => PACKAGING_INPUT.test(path)),
  };
}

function main() {
  const [base, head] = process.argv.slice(2);
  if (!base || !head) {
    console.error("usage: classify-changes.mjs <base-sha> <head-sha>");
    process.exitCode = 2;
    return;
  }
  const files = execFileSync("git", ["diff", "--name-only", base, head], {
    encoding: "utf8",
  }).trim().split("\n").filter(Boolean);
  files.forEach((file) => console.log(file));
  const result = classifyChanges(files);
  const output = [
    `macos_required=${result.macosRequired}`,
    `package_required=${result.packageRequired}`,
  ].join("\n");
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `${output}\n`);
  else console.log(output);
}

if (import.meta.url === `file://${process.argv[1]}`) main();
