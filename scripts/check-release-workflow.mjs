#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { chmodSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { assemble, expectedAssetNames } from "./release/assemble.mjs";
import { validateReleaseConfiguration } from "./release/preflight.mjs";

const root = resolve(import.meta.dirname, "..");
const workflow = readFileSync(resolve(root, ".github/workflows/release.yml"), "utf8");
const packageCli = readFileSync(resolve(root, "scripts/release/package-cli.sh"), "utf8");
const buildMacos = readFileSync(resolve(root, "scripts/release/build-macos-targets.sh"), "utf8");
const releaseTauri = JSON.parse(readFileSync(resolve(root, "apps/desktop/src-tauri/tauri.release.conf.json"), "utf8"));
const verifyAttestations = resolve(root, "scripts/release/verify-attestations.sh");
const errors = [];

function requireText(text, message) {
  if (!workflow.includes(text)) errors.push(message);
}

requireText("workflow_dispatch:", "release workflow must support explicit rehearsal/publish dispatch");
requireText('tags:\n      - "v[0-9]+.[0-9]+.[0-9]+"', "release workflow must use strict stable SemVer tags");
requireText("permissions:\n  contents: read", "release workflow must default to contents:read");
requireText("'release-signing'", "release signing secrets must use a protected environment");
requireText("environment: release-publish", "release publication must use a separate protected environment");
requireText("name: release-attestation", "release artifacts must receive provenance attestations");
requireText("name: release-publish", "release publication must be a distinct job");
requireText("--draft --generate-notes", "release publication must create a draft first");
requireText("published versions are immutable", "release publication must reject replacement releases");
requireText("minisign -Vm", "updater archives must be verified with the checked-in public key");
requireText(
  'scripts/release/verify-attestations.sh release-assets "$GITHUB_REPOSITORY" "$GITHUB_SHA"',
  "publish must verify each downloaded artifact's provenance",
);
requireText("sha256sum --check SHA256SUMS", "publish must verify checksums after downloading artifacts");
const publishJob = workflow.split(/^  publish:\s*$/mu)[1] ?? "";
if (!/^\s{6}attestations: read\s*$/mu.test(publishJob)) {
  errors.push("release publication must have read access to provenance attestations");
}
if (!publishJob.includes("Check out release verification scripts")) {
  errors.push("release publication must check out the repository before running verification scripts");
}
requireText("/commits/$GITHUB_SHA/pulls", "publish preflight must resolve the merged PR for a squash commit");
requireText("head_sha=$pr_head", "publish preflight must verify CI for the merged PR head");
requireText("name: cli-linux", "Linux CLI architectures must share one runner");
requireText("name: macos-universal-release-set", "macOS release architectures must share one runner");
if (!buildMacos.includes("AIZU_BUNDLED_CLI_TARGET=$target")) {
  errors.push("macOS app bundles must embed the matching CLI architecture");
}
for (const expected of ["scripts/release/build-macos-dmg.sh", "xcrun notarytool submit", "xcrun stapler staple"]) {
  if (!buildMacos.includes(expected)) errors.push(`macOS publication is missing canonical packaging step: ${expected}`);
}
if (releaseTauri.bundle?.targets?.join(",") !== "app") {
  errors.push("Tauri release bundling must leave DMG creation to the canonical Finder-alias builder");
}
if (!packageCli.includes("gzip -9n")) errors.push("standalone CLI archives must omit variable gzip timestamps");
if (/^\s{4}(?:cli|macos):[\s\S]*?^\s{4}strategy:/mu.test(workflow)) {
  errors.push("release CLI and macOS builds must not expand architecture runner matrices");
}
const topLevel = workflow.split(/^jobs:\s*$/mu)[0] ?? workflow;
if (/^\s{2}contents: write\s*$/mu.test(topLevel)) {
  errors.push("release workflow must not grant contents:write at top level");
}
if (workflow.includes("pull_request_target")) errors.push("release workflow must not use pull_request_target");
if (/uses:\s+[^\s]+@(?![0-9a-f]{40}(?:\s|$))/u.test(workflow)) {
  errors.push("release workflow actions must be pinned to full commit SHAs");
}

const fixture = {
  mode: "rehearsal",
  version: "1.2.3",
  refType: "branch",
  refName: "main",
  cargo: { "aizu-core": "1.2.3", "aizu-cli": "1.2.3", "aizu-desktop": "1.2.3" },
  desktopPackage: { version: "1.2.3" },
  tauri: { version: "1.2.3", bundle: {}, plugins: {} },
  releaseTauri: { bundle: {} },
  branding: { branding_status: "development-approved", release_approved: false },
};
if (validateReleaseConfiguration(fixture).length !== 0) errors.push("rehearsal preflight should accept development configuration");
const publishErrors = validateReleaseConfiguration({ ...fixture, mode: "publish", refType: "tag", refName: "v1.2.3" });
for (const expected of ["approved branding", "createUpdaterArtifacts", "updater public key"]) {
  if (!publishErrors.some((error) => error.includes(expected))) errors.push(`publish preflight is missing ${expected} failure`);
}
const readyPublish = validateReleaseConfiguration({
  ...fixture,
  mode: "publish",
  refType: "tag",
  refName: "v1.2.3",
  branding: { branding_status: "approved", release_approved: true },
  releaseTauri: { bundle: { createUpdaterArtifacts: true } },
  tauri: { ...fixture.tauri, plugins: { updater: { pubkey: `RWT${"A".repeat(41)}` } } },
});
if (readyPublish.length !== 0) errors.push(`release-ready fixture was rejected: ${readyPublish.join(", ")}`);
const malformedKey = validateReleaseConfiguration({
  ...fixture,
  mode: "publish",
  refType: "tag",
  refName: "v1.2.3",
  branding: { branding_status: "approved", release_approved: true },
  releaseTauri: { bundle: { createUpdaterArtifacts: true } },
  tauri: { ...fixture.tauri, plugins: { updater: { pubkey: "x".repeat(64) } } },
});
if (!malformedKey.some((error) => error.includes("updater public key"))) {
  errors.push("publish preflight must reject malformed updater public keys");
}
if (!validateReleaseConfiguration({ ...fixture, version: "1.2" }).some((error) => error.includes("SemVer"))) {
  errors.push("release preflight must reject non-SemVer versions");
}
for (const version of ["1.2.3-rc.1", "1.2.3+build.1"]) {
  const unstableErrors = validateReleaseConfiguration({
    ...readyPublish,
    mode: "publish",
    version,
    refType: "tag",
    refName: `v${version}`,
    cargo: { "aizu-core": version, "aizu-cli": version, "aizu-desktop": version },
    desktopPackage: { version },
    tauri: { ...readyPublish.tauri, version },
  });
  if (!unstableErrors.some((error) => error.includes("stable X.Y.Z"))) {
    errors.push(`release preflight must reject unstable public version ${version}`);
  }
}

const directory = mkdtempSync(resolve(tmpdir(), "aizu-release-contract-"));
try {
  for (const name of expectedAssetNames("1.2.3", false)) writeFileSync(resolve(directory, name), `${name}\n`);
  await assemble({
    directory,
    version: "1.2.3",
    publish: false,
    repository: "example/aizu",
    publishedAt: "2026-01-01T00:00:00Z",
  });
  execFileSync(process.execPath, [resolve(root, "scripts/release/verify.mjs"), directory], { stdio: "ignore" });
  writeFileSync(resolve(directory, expectedAssetNames("1.2.3", false)[0]), "tampered\n");
  try {
    execFileSync(process.execPath, [resolve(root, "scripts/release/verify.mjs"), directory], { stdio: "ignore" });
    errors.push("release checksum verification must reject modified artifacts");
  } catch {
    // Expected.
  }
  writeFileSync(resolve(directory, "unlisted.txt"), "unexpected\n");
  try {
    execFileSync(process.execPath, [resolve(root, "scripts/release/verify.mjs"), directory], { stdio: "ignore" });
    errors.push("release checksum verification must reject unlisted artifacts");
  } catch {
    // Expected.
  }
} finally {
  rmSync(directory, { force: true, recursive: true });
}

const attestationDirectory = mkdtempSync(resolve(tmpdir(), "aizu-release-attestations-"));
const mockBin = mkdtempSync(resolve(tmpdir(), "aizu-release-gh-"));
try {
  writeFileSync(resolve(attestationDirectory, "first artifact"), "first\n");
  writeFileSync(resolve(attestationDirectory, "second-artifact"), "second\n");
  const mockGh = resolve(mockBin, "gh");
  writeFileSync(
    mockGh,
    `#!/usr/bin/env bash
set -euo pipefail
[[ $# -eq 9 && $1 == attestation && $2 == verify && $4 == --repo && $5 == example/aizu ]] || exit 64
[[ $6 == --signer-workflow && $7 == example/aizu/.github/workflows/release.yml ]] || exit 65
[[ $8 == --source-digest && $9 == 0123456789abcdef0123456789abcdef01234567 ]] || exit 66
printf '%s\\n' "$3" >> "$AIZU_ATTESTATION_LOG"
`,
  );
  chmodSync(mockGh, 0o755);
  const log = resolve(mockBin, "calls.log");
  execFileSync(verifyAttestations, [attestationDirectory, "example/aizu", "0123456789abcdef0123456789abcdef01234567"], {
    env: { ...process.env, AIZU_ATTESTATION_LOG: log, PATH: `${mockBin}:${process.env.PATH ?? ""}` },
    stdio: "ignore",
  });
  const calls = readFileSync(log, "utf8").trim().split("\n");
  if (calls.length !== 2 || !calls.every((call) => call.startsWith(`${attestationDirectory}/`))) {
    errors.push("release publication must verify every artifact with one gh invocation per file");
  }
} catch {
  errors.push("release attestation verification contract failed");
} finally {
  rmSync(attestationDirectory, { force: true, recursive: true });
  rmSync(mockBin, { force: true, recursive: true });
}

const packageDirectory = mkdtempSync(resolve(tmpdir(), "aizu-release-package-"));
try {
  const target = "contract-target";
  const binaryDirectory = resolve(root, "target", target, "release");
  const binary = resolve(binaryDirectory, "aizu");
  const first = resolve(packageDirectory, "first");
  const second = resolve(packageDirectory, "second");
  mkdirSync(binaryDirectory, { recursive: true });
  writeFileSync(binary, "contract binary\n");
  chmodSync(binary, 0o755);
  execFileSync(resolve(root, "scripts/release/package-cli.sh"), [target, "test", "x64", "1.2.3", first], {
    stdio: "ignore",
  });
  execFileSync(resolve(root, "scripts/release/package-cli.sh"), [target, "test", "x64", "1.2.3", second], {
    stdio: "ignore",
  });
  const archive = "aizu-cli_1.2.3_test-x64.tar.gz";
  if (!readFileSync(resolve(first, archive)).equals(readFileSync(resolve(second, archive)))) {
    errors.push("standalone CLI archives must be byte-for-byte deterministic");
  }
} finally {
  rmSync(resolve(root, "target", "contract-target"), { force: true, recursive: true });
  rmSync(packageDirectory, { force: true, recursive: true });
}

if (errors.length > 0) {
  errors.forEach((error) => console.error(error));
  process.exitCode = 1;
} else {
  console.log("validated fail-closed release workflow, preflight, inventory, and checksums");
}
