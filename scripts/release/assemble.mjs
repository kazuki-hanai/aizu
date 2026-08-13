#!/usr/bin/env node

import { createHash } from "node:crypto";
import { createReadStream, existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, resolve } from "node:path";

const SEMVER = /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/u;

async function sha256(path) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}

export function expectedAssetNames(version, publish) {
  const names = [
    `Aizu_${version}_aarch64.dmg`,
    `Aizu_${version}_x64.dmg`,
    `aizu-cli_${version}_linux-aarch64.tar.gz`,
    `aizu-cli_${version}_linux-x64.tar.gz`,
    `aizu-cli_${version}_macos-aarch64.tar.gz`,
    `aizu-cli_${version}_macos-x64.tar.gz`,
    "SBOM.spdx.json",
  ];
  if (publish) {
    for (const arch of ["aarch64", "x64"]) {
      names.push(`Aizu_${version}_${arch}.app.tar.gz`);
      names.push(`Aizu_${version}_${arch}.app.tar.gz.sig`);
    }
  }
  return names.sort();
}

export async function assemble({ directory, version, publish, repository, publishedAt }) {
  if (!SEMVER.test(version)) throw new Error(`invalid release version: ${version}`);
  const expected = expectedAssetNames(version, publish);
  for (const name of expected) {
    if (!existsSync(resolve(directory, name))) throw new Error(`missing release asset: ${name}`);
  }
  const current = readdirSync(directory, { withFileTypes: true })
    .filter((entry) => entry.isFile())
    .map(({ name }) => name)
    .sort();
  const unexpected = current.filter((name) => !expected.includes(name));
  if (unexpected.length > 0) throw new Error(`unexpected release assets: ${unexpected.join(", ")}`);

  if (publish) {
    const platforms = {};
    for (const [platform, arch] of [["darwin-aarch64", "aarch64"], ["darwin-x86_64", "x64"]]) {
      const archive = `Aizu_${version}_${arch}.app.tar.gz`;
      const signature = readFileSync(resolve(directory, `${archive}.sig`), "utf8").trim();
      if (signature.length < 32) throw new Error(`invalid updater signature: ${archive}.sig`);
      platforms[platform] = {
        signature,
        url: `https://github.com/${repository}/releases/download/v${version}/${archive}`,
      };
    }
    writeFileSync(resolve(directory, "latest.json"), `${JSON.stringify({
      version,
      notes: `Aizu ${version}`,
      pub_date: publishedAt,
      platforms,
    }, null, 2)}\n`);
    expected.push("latest.json");
  }

  const checksums = [];
  for (const name of expected.sort()) {
    checksums.push(`${await sha256(resolve(directory, name))}  ${name}`);
  }
  writeFileSync(resolve(directory, "SHA256SUMS"), `${checksums.join("\n")}\n`);
}

async function main() {
  const [directory, version, mode, repository, publishedAt] = process.argv.slice(2);
  if (!directory || !version || !mode || !repository || !publishedAt) {
    throw new Error("usage: assemble.mjs <directory> <version> <rehearsal|publish> <owner/repo> <ISO-date>");
  }
  await assemble({
    directory: resolve(directory),
    version,
    publish: mode === "publish",
    repository,
    publishedAt,
  });
  console.log(`assembled ${basename(directory)} for Aizu ${version}`);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
