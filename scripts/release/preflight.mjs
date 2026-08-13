#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { appendFileSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SEMVER = /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/u;
const STABLE_SEMVER = /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/u;
const root = resolve(fileURLToPath(new URL("../..", import.meta.url)));

function readJson(path) {
  return JSON.parse(readFileSync(resolve(root, path), "utf8"));
}

function cargoVersions() {
  const metadata = JSON.parse(execFileSync(
    "cargo",
    ["metadata", "--locked", "--no-deps", "--format-version", "1"],
    { cwd: root, encoding: "utf8" },
  ));
  return Object.fromEntries(
    metadata.packages
      .filter(({ name }) => ["aizu-core", "aizu-cli", "aizu-desktop"].includes(name))
      .map(({ name, version }) => [name, version]),
  );
}

export function validateReleaseConfiguration({
  mode,
  version,
  refType,
  refName,
  cargo = cargoVersions(),
  desktopPackage = readJson("apps/desktop/package.json"),
  tauri = readJson("apps/desktop/src-tauri/tauri.conf.json"),
  releaseTauri = readJson("apps/desktop/src-tauri/tauri.release.conf.json"),
  branding = readJson("assets/branding/icon-manifest.json"),
}) {
  const errors = [];
  if (!['rehearsal', 'publish'].includes(mode)) errors.push(`unsupported release mode: ${mode}`);
  if (!SEMVER.test(version)) errors.push(`release version is not SemVer: ${version}`);

  for (const name of ["aizu-core", "aizu-cli", "aizu-desktop"]) {
    if (cargo[name] !== version) errors.push(`${name} version ${cargo[name] ?? "missing"} does not match ${version}`);
  }
  if (desktopPackage.version !== version) {
    errors.push(`desktop package version ${desktopPackage.version ?? "missing"} does not match ${version}`);
  }
  if (tauri.version !== version) {
    errors.push(`Tauri version ${tauri.version ?? "missing"} does not match ${version}`);
  }

  if (mode === "publish") {
    if (!STABLE_SEMVER.test(version)) {
      errors.push(`public release version must be stable X.Y.Z SemVer: ${version}`);
    }
    if (refType !== "tag" || refName !== `v${version}`) {
      errors.push(`publish must run from the exact v${version} tag`);
    }
    if (branding.branding_status !== "approved" || branding.release_approved !== true) {
      errors.push("public release requires an approved branding manifest");
    }
    if (releaseTauri.bundle?.createUpdaterArtifacts !== true) {
      errors.push("public release requires bundle.createUpdaterArtifacts=true");
    }
    const updater = tauri.plugins?.updater;
    const updaterKey = typeof updater?.pubkey === "string" ? updater.pubkey.trim() : "";
    if (!/^RWT[0-9A-Za-z+/]{39,}={0,2}$/u.test(updaterKey)) {
      errors.push("public release requires a non-placeholder updater public key");
    }
  }
  return errors;
}

function main() {
  const mode = process.env.RELEASE_MODE ?? "";
  const version = process.env.RELEASE_VERSION ?? "";
  const refType = process.env.RELEASE_REF_TYPE ?? "";
  const refName = process.env.RELEASE_REF_NAME ?? "";
  const sha = process.env.RELEASE_SHA ?? "";
  const errors = validateReleaseConfiguration({ mode, version, refType, refName });

  if (!/^[0-9a-f]{40}$/u.test(sha)) errors.push("release SHA must be a full commit SHA");
  try {
    execFileSync("git", ["merge-base", "--is-ancestor", sha, "origin/main"], {
      cwd: root,
      stdio: "ignore",
    });
  } catch {
    errors.push("release commit is not contained in origin/main");
  }
  if (mode === "publish") {
    try {
      const tagged = execFileSync("git", ["rev-list", "-n", "1", `refs/tags/v${version}`], {
        cwd: root,
        encoding: "utf8",
      }).trim();
      if (tagged !== sha) errors.push(`v${version} does not point at the release commit`);
    } catch {
      errors.push(`v${version} is not available in the checked-out repository`);
    }
  }

  if (errors.length > 0) {
    errors.forEach((error) => console.error(`release preflight: ${error}`));
    process.exitCode = 1;
    return;
  }

  const output = [
    `version=${version}`,
    `tag=v${version}`,
    `publish=${String(mode === "publish")}`,
  ].join("\n");
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `${output}\n`);
  else console.log(output);
}

if (import.meta.url === `file://${process.argv[1]}`) main();
