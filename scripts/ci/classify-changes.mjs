#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { appendFileSync } from "node:fs";

const DOC_ONLY = /\.md$/u;
const PACKAGING_INPUT = /^(?:Cargo\.(?:toml|lock)|package\.json|pnpm-lock\.yaml|mise\.(?:toml|lock)|\.github\/workflows\/(?:ci|release)\.yml|assets\/(?:audio|branding)\/|apps\/desktop\/(?:package\.json|src-tauri\/(?:Cargo\.toml|build\.rs|icons\/|resources\/|tauri(?:\.release)?\.conf\.json))|scripts\/(?:build-dev-dmg|install-dev-app|prepare-desktop-cli|verify-audio-resources)\.sh|scripts\/(?:ci\/verify-dev-dmg\.sh|release\/))/u;

export function changedPaths(base, head, cwd = process.cwd()) {
  const fields = execFileSync("git", ["diff", "--name-status", "-z", `${base}...${head}`], {
    cwd,
    encoding: "utf8",
  }).split("\0");
  const paths = [];
  for (let index = 0; index < fields.length && fields[index];) {
    const status = fields[index++];
    const pathCount = /^[RC]/u.test(status) ? 2 : 1;
    for (let offset = 0; offset < pathCount; offset += 1) {
      const path = fields[index++];
      if (path) paths.push(path);
    }
  }
  return [...new Set(paths)];
}

export function classifyChanges(paths) {
  const files = paths.filter(Boolean);
  return {
    macosRequired: files.some((path) => !DOC_ONLY.test(path)),
    packageRequired: files.some((path) => !DOC_ONLY.test(path) && PACKAGING_INPUT.test(path)),
  };
}

function main() {
  const [base, head] = process.argv.slice(2);
  if (!base || !head) {
    console.error("usage: classify-changes.mjs <base-sha> <head-sha>");
    process.exitCode = 2;
    return;
  }
  const files = changedPaths(base, head);
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
