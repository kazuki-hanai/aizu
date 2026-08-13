#!/usr/bin/env node

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { inflateSync } from "node:zlib";

const REQUIRED_NODE_VERSION = "v24.19.0";
const ROOT = resolve(import.meta.dirname, "..");
if (process.version !== REQUIRED_NODE_VERSION) {
  throw new Error(`icon inspection requires Node ${REQUIRED_NODE_VERSION}; got ${process.version}`);
}

function read(relativePath) {
  return readFileSync(resolve(ROOT, relativePath));
}

function sha256(data) {
  return createHash("sha256").update(data).digest("hex");
}

function parsePng(data, label) {
  if (!data.subarray(0, 8).equals(Buffer.from("89504e470d0a1a0a", "hex"))) {
    throw new Error(`${label}: invalid PNG signature`);
  }
  let offset = 8;
  let width;
  let height;
  let colorType;
  let hasSrgb = false;
  const imageData = [];
  while (offset < data.length) {
    const length = data.readUInt32BE(offset);
    const type = data.toString("ascii", offset + 4, offset + 8);
    const payload = data.subarray(offset + 8, offset + 8 + length);
    if (type === "IHDR") {
      width = payload.readUInt32BE(0);
      height = payload.readUInt32BE(4);
      colorType = payload[9];
      if (payload[8] !== 8) throw new Error(`${label}: PNG must use 8-bit channels`);
    } else if (type === "sRGB") {
      hasSrgb = payload.length === 1 && payload[0] === 0;
    } else if (type === "IDAT") {
      imageData.push(payload);
    } else if (type === "IEND") {
      break;
    }
    offset += 12 + length;
  }
  if (!width || !height || colorType !== 6 || !hasSrgb) {
    throw new Error(`${label}: expected RGBA PNG with explicit perceptual sRGB profile`);
  }
  const scanlines = inflateSync(Buffer.concat(imageData));
  const rowLength = 1 + width * 4;
  if (scanlines.length !== rowLength * height) throw new Error(`${label}: invalid image-data length`);
  const pixels = Buffer.alloc(width * height * 4);
  for (let y = 0; y < height; y += 1) {
    if (scanlines[y * rowLength] !== 0) throw new Error(`${label}: unsupported PNG filter`);
    scanlines.copy(pixels, y * width * 4, y * rowLength + 1, (y + 1) * rowLength);
  }
  return { width, height, pixels };
}

function inspectAppPng(relativePath, width, height) {
  const parsed = parsePng(read(relativePath), relativePath);
  if (parsed.width !== width || parsed.height !== height) {
    throw new Error(`${relativePath}: expected ${width}x${height}, got ${parsed.width}x${parsed.height}`);
  }
  if (!parsed.pixels.some((_, index) => index % 4 === 3)) {
    throw new Error(`${relativePath}: missing alpha samples`);
  }
}

function inspectTrayPng(relativePath, size) {
  const parsed = parsePng(read(relativePath), relativePath);
  if (parsed.width !== size || parsed.height !== size) {
    throw new Error(`${relativePath}: expected ${size}x${size}, got ${parsed.width}x${parsed.height}`);
  }
  let transparent = false;
  let visible = false;
  for (let offset = 0; offset < parsed.pixels.length; offset += 4) {
    const red = parsed.pixels[offset];
    const green = parsed.pixels[offset + 1];
    const blue = parsed.pixels[offset + 2];
    const alpha = parsed.pixels[offset + 3];
    transparent ||= alpha === 0;
    visible ||= alpha > 0;
    if (alpha > 0 && (red !== 0 || green !== 0 || blue !== 0)) {
      throw new Error(`${relativePath}: tray template contains a non-black visible pixel`);
    }
  }
  if (!transparent || !visible) throw new Error(`${relativePath}: tray template needs visible and transparent pixels`);
}

const manifest = JSON.parse(read("assets/branding/icon-manifest.json").toString("utf8"));
if (manifest.schema_version !== 1 || manifest.branding_status !== "development-approved") {
  throw new Error("icon-manifest.json: invalid development branding status");
}
if (manifest.release_approved !== false) {
  throw new Error("development artwork must not claim release approval");
}
if (manifest.generator.runtime !== "node 24.19.0" || manifest.color_profile !== "sRGB") {
  throw new Error("icon-manifest.json: generator runtime or color profile is not pinned");
}
if (!Array.isArray(manifest.forbidden_default_icon_fingerprints.sha256)
    || manifest.forbidden_default_icon_fingerprints.sha256.length === 0) {
  throw new Error("icon-manifest.json: missing Tauri default icon fingerprints");
}

for (const output of manifest.outputs) {
  const data = read(output.path);
  if (sha256(data) !== output.sha256) throw new Error(`${output.path}: manifest hash mismatch`);
  if (manifest.forbidden_default_icon_fingerprints.sha256.includes(output.sha256)) {
    throw new Error(`${output.path}: forbidden Tauri default icon fingerprint`);
  }
}

inspectAppPng("assets/branding/app-icon/app-icon-1024.png", 1024, 1024);
inspectAppPng("apps/desktop/src-tauri/icons/32x32.png", 32, 32);
inspectAppPng("apps/desktop/src-tauri/icons/128x128.png", 128, 128);
inspectAppPng("apps/desktop/src-tauri/icons/128x128@2x.png", 256, 256);

for (const state of ["normal", "attention", "paused", "error"]) {
  inspectTrayPng(`apps/desktop/src-tauri/icons/tray/tray-${state}.png`, 18);
  inspectTrayPng(`apps/desktop/src-tauri/icons/tray/tray-${state}@2x.png`, 36);
}

const icns = read("apps/desktop/src-tauri/icons/icon.icns");
if (icns.toString("ascii", 0, 4) !== "icns" || icns.readUInt32BE(4) !== icns.length) {
  throw new Error("icon.icns: invalid ICNS container");
}
const icnsTypes = [];
for (let offset = 8; offset < icns.length;) {
  const type = icns.toString("ascii", offset, offset + 4);
  const length = icns.readUInt32BE(offset + 4);
  if (length < 8 || offset + length > icns.length) throw new Error("icon.icns: invalid chunk length");
  icnsTypes.push(type);
  parsePng(icns.subarray(offset + 8, offset + length), `icon.icns:${type}`);
  offset += length;
}
if (icnsTypes.join(",") !== "ic07,ic08,ic09,ic10") throw new Error("icon.icns: incomplete representations");

const ico = read("apps/desktop/src-tauri/icons/icon.ico");
if (ico.readUInt16LE(0) !== 0 || ico.readUInt16LE(2) !== 1 || ico.readUInt16LE(4) !== 3) {
  throw new Error("icon.ico: invalid ICO directory");
}
for (let index = 0; index < 3; index += 1) {
  const entry = 6 + index * 16;
  const length = ico.readUInt32LE(entry + 8);
  const offset = ico.readUInt32LE(entry + 12);
  parsePng(ico.subarray(offset, offset + length), `icon.ico:${index}`);
}

console.log("validated PNG dimensions, RGBA/sRGB data, tray monochrome alpha, ICNS, ICO, and fingerprints");
