#!/usr/bin/env node

import { readFileSync } from "node:fs";

const MAX_ENCODED_SIGNATURE_BYTES = 4_096;

function decodeCanonicalBase64(value, expectedBytes) {
  if (!/^[0-9A-Za-z+/]+={0,2}$/u.test(value) || value.length % 4 !== 0) return null;
  const decoded = Buffer.from(value, "base64");
  if (decoded.length !== expectedBytes || decoded.toString("base64") !== value) return null;
  return decoded;
}

export function decodeUpdaterSignature(encoded) {
  if (typeof encoded !== "string"
      || Buffer.byteLength(encoded) > MAX_ENCODED_SIGNATURE_BYTES
      || encoded.trim() !== encoded
      || encoded.length === 0
      || encoded.length % 4 !== 0
      || !/^[0-9A-Za-z+/]+={0,2}$/u.test(encoded)) {
    return null;
  }

  const decodedBytes = Buffer.from(encoded, "base64");
  if (decodedBytes.toString("base64") !== encoded) return null;
  const decoded = decodedBytes.toString("utf8");
  if (!Buffer.from(decoded, "utf8").equals(decodedBytes) || !decoded.endsWith("\n")) return null;

  const lines = decoded.slice(0, -1).split("\n");
  if (lines.length !== 4
      || lines[0] !== "untrusted comment: signature from tauri secret key"
      || !lines[2].startsWith("trusted comment: timestamp:")) {
    return null;
  }
  const signature = decodeCanonicalBase64(lines[1], 74);
  const globalSignature = decodeCanonicalBase64(lines[3], 64);
  if (signature === null
      || globalSignature === null
      || signature[0] !== 0x45
      || ![0x44, 0x64].includes(signature[1])) {
    return null;
  }
  return decoded;
}

function main() {
  const [path] = process.argv.slice(2);
  if (!path || process.argv.length !== 3) {
    console.error("usage: decode-updater-signature.mjs <signature-path>");
    process.exitCode = 2;
    return;
  }
  const encoded = readFileSync(path, "utf8");
  const decoded = decodeUpdaterSignature(encoded);
  if (decoded === null) {
    console.error("updater signature is not a canonical Tauri minisign envelope");
    process.exitCode = 1;
  } else {
    process.stdout.write(decoded);
  }
}

if (import.meta.url === `file://${process.argv[1]}`) main();
