#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { changedPaths, classifyChanges } from "./ci/classify-changes.mjs";

const root = resolve(import.meta.dirname, "..");
const ci = readFileSync(resolve(root, ".github/workflows/ci.yml"), "utf8");
const weekly = readFileSync(resolve(root, ".github/workflows/nightly.yml"), "utf8");
const errors = [];

function topLevelJobNames(workflow) {
  const jobs = workflow.split(/^jobs:\s*$/mu)[1] ?? "";
  return [...jobs.matchAll(/^  ([a-zA-Z0-9_-]+):\s*$/gmu)].map((match) => match[1]);
}

const pullRequestJobs = topLevelJobNames(ci);
if (pullRequestJobs.join(",") !== "quality,macos") {
  errors.push(`PR workflow must contain only quality and macos jobs; found ${pullRequestJobs.join(", ")}`);
}
if (!/^\s{2}pull_request:\s*$/mu.test(ci)) errors.push("PR workflow must run for pull_request events");
if (/^\s{2}push:\s*$/mu.test(ci)) errors.push("PR workflow must not rerun the same commit after merge");
if (!ci.includes("cancel-in-progress: true")) errors.push("PR workflow must cancel superseded runs");
if (!ci.includes("steps.changes.outputs.macos_required == 'true'")) {
  errors.push("macOS runner must be skipped for documentation-only changes");
}
if (!ci.includes("if: needs.quality.outputs.macos_required == 'true'")) {
  errors.push("macOS aggregate job must use the documentation-only change decision");
}
const classifications = [
  { files: ["README.md", "docs/protocol.md"], macos: false, package: false },
  { files: [".github/PULL_REQUEST_TEMPLATE.md"], macos: false, package: false },
  { files: ["assets/branding/README.md"], macos: false, package: false },
  { files: ["crates/aizu-core/src/lib.rs"], macos: true, package: false },
  { files: ["apps/desktop/src/App.tsx"], macos: true, package: false },
  { files: ["apps/desktop/src-tauri/tauri.conf.json"], macos: true, package: true },
  { files: ["assets/audio/aizu-pop.wav"], macos: true, package: true },
  { files: ["package.json"], macos: true, package: true },
  { files: ["pnpm-lock.yaml"], macos: true, package: true },
  { files: ["apps/desktop/package.json"], macos: true, package: true },
  { files: ["mise.toml"], macos: true, package: true },
  { files: [".github/workflows/ci.yml"], macos: true, package: true },
];
for (const expected of classifications) {
  const actual = classifyChanges(expected.files);
  if (actual.macosRequired !== expected.macos || actual.packageRequired !== expected.package) {
    errors.push(`unexpected change classification for ${expected.files.join(", ")}`);
  }
}
if (existsSync(resolve(root, ".github/workflows/branding.yml"))) {
  errors.push("branding checks must stay in the aggregate quality job");
}

const fixture = mkdtempSync(resolve(tmpdir(), "aizu-ci-budget-"));
try {
  const git = (...args) => execFileSync("git", args, { cwd: fixture, stdio: "ignore" });
  git("init", "--quiet");
  git("config", "user.email", "ci-budget@example.invalid");
  git("config", "user.name", "CI Budget Test");
  writeFileSync(resolve(fixture, "README.md"), "initial\n");
  mkdirSync(resolve(fixture, "crates"));
  writeFileSync(resolve(fixture, "crates/core.rs"), "initial\n");
  git("add", ".");
  git("commit", "--quiet", "-m", "initial");
  git("branch", "pr-head");
  writeFileSync(resolve(fixture, "crates/core.rs"), "base advanced\n");
  git("commit", "--quiet", "-am", "base code change");
  const base = execFileSync("git", ["rev-parse", "HEAD"], { cwd: fixture, encoding: "utf8" }).trim();
  git("checkout", "--quiet", "pr-head");
  writeFileSync(resolve(fixture, "README.md"), "documentation only\n");
  git("commit", "--quiet", "-am", "PR docs change");
  const head = execFileSync("git", ["rev-parse", "HEAD"], { cwd: fixture, encoding: "utf8" }).trim();
  const paths = changedPaths(base, head, fixture);
  const classification = classifyChanges(paths);
  if (paths.join(",") !== "README.md" || classification.macosRequired) {
    errors.push(`PR changes must use the merge base; found ${paths.join(", ")}`);
  }
} finally {
  rmSync(fixture, { force: true, recursive: true });
}

const weeklyJobs = topLevelJobNames(weekly);
if (weeklyJobs.join(",") !== "platform") {
  errors.push(`weekly workflow must use one platform matrix job; found ${weeklyJobs.join(", ")}`);
}
if (!weekly.includes('cron: "17 3 * * 1"')) errors.push("deep checks must run weekly, not daily");
for (const platform of ["linux", "macos", "windows"]) {
  if (!weekly.includes(`platform: ${platform}`)) errors.push(`weekly matrix is missing ${platform}`);
}
const weeklyPlatforms = [...weekly.matchAll(/^\s+- platform: ([a-z0-9_-]+)\s*$/gmu)].map(
  (match) => match[1],
);
if (weeklyPlatforms.join(",") !== "linux,macos,windows") {
  errors.push(`weekly matrix must contain exactly linux, macos, and windows; found ${weeklyPlatforms.join(", ")}`);
}
if (!weekly.includes("cancel-in-progress: true")) errors.push("weekly checks must not overlap");

if (errors.length > 0) {
  errors.forEach((error) => console.error(error));
  process.exitCode = 1;
} else {
  console.log("validated Actions budget: 2 PR runners, docs-only macOS skip, 3 weekly runners");
}
