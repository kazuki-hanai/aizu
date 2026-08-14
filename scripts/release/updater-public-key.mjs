#!/usr/bin/env node

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { decodeUpdaterPublicKey } from "./preflight.mjs";

const root = resolve(fileURLToPath(new URL("../..", import.meta.url)));
const tauri = JSON.parse(
  readFileSync(resolve(root, "apps/desktop/src-tauri/tauri.conf.json"), "utf8"),
);
const key = decodeUpdaterPublicKey(tauri.plugins?.updater?.pubkey);
if (key === null) {
  console.error("configured Tauri updater public key is invalid");
  process.exitCode = 1;
} else {
  process.stdout.write(key);
}
