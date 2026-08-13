#!/usr/bin/env node

import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { deflateSync } from "node:zlib";

const GENERATOR_VERSION = "1.0.0";
const REQUIRED_NODE_VERSION = "v24.19.0";
const ROOT = resolve(import.meta.dirname, "..");
const CHECK_MODE = process.argv.includes("--check");

if (process.version !== REQUIRED_NODE_VERSION) {
  throw new Error(
    `icon generation requires Node ${REQUIRED_NODE_VERSION}; got ${process.version}`,
  );
}

const sourcePaths = [
  "assets/branding/app-icon/layers/background.svg",
  "assets/branding/app-icon/layers/foreground.svg",
  "assets/branding/tray/tray-normal.svg",
  "assets/branding/tray/tray-attention.svg",
  "assets/branding/tray/tray-paused.svg",
  "assets/branding/tray/tray-error.svg",
];

const forbiddenFingerprints = {
  source:
    "tauri-apps/create-tauri-app@0adb54c49e7284955f254783a78fff2cb9de6696",
  sha256: [
    "d151f11e325f7502de0c739a2e51697aa569fd4701ae6e11fae1a3b4c7d5f157",
    "1f3689f6374b0553996fdc99743799216703835070145d7f0e6ec11e6280139e",
    "69194785eef4323955af73b0c03362ac0804db056c2424f9715076cabc0ed103",
    "3dc10493b7de48a61de58f768f8a5708d3a44a068c148cedf0502b9b9b71ba5d",
    "e38ca88e1d5490f3dcbc3c3fa525f7fcb7b80fff3cb2f3a4eb1b2d018c0915c1",
    "b5d93c8ec365c08b11bd006e46a46e227c681d30bb295af3d017573bfc752a83",
  ],
};

function sha256(data) {
  return createHash("sha256").update(data).digest("hex");
}

function read(relativePath) {
  return readFileSync(resolve(ROOT, relativePath));
}

function attributes(raw) {
  const parsed = {};
  for (const match of raw.matchAll(/([\w-]+)="([^"]*)"/gu)) {
    parsed[match[1]] = match[2];
  }
  return parsed;
}

function parseColor(value) {
  if (!/^#[0-9A-Fa-f]{6}$/u.test(value)) {
    throw new Error(`unsupported SVG color: ${value}`);
  }
  return [
    Number.parseInt(value.slice(1, 3), 16),
    Number.parseInt(value.slice(3, 5), 16),
    Number.parseInt(value.slice(5, 7), 16),
    255,
  ];
}

function number(value, label) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) {
    throw new Error(`invalid ${label}: ${value}`);
  }
  return parsed;
}

function parseSvg(relativePath) {
  const text = read(relativePath).toString("utf8");
  const root = text.match(/<svg\b([^>]*)>/u);
  if (!root) throw new Error(`${relativePath}: missing svg root`);
  const rootAttributes = attributes(root[1]);
  const width = number(rootAttributes.width, "SVG width");
  const height = number(rootAttributes.height, "SVG height");
  const shapes = [];

  for (const match of text.matchAll(/<(rect|circle|polygon)\b([^>]*)\/>/gu)) {
    const type = match[1];
    const attrs = attributes(match[2]);
    const fill = parseColor(attrs.fill);
    if (type === "rect") {
      shapes.push({
        type,
        x: number(attrs.x, "rect x"),
        y: number(attrs.y, "rect y"),
        width: number(attrs.width, "rect width"),
        height: number(attrs.height, "rect height"),
        fill,
      });
    } else if (type === "circle") {
      shapes.push({
        type,
        cx: number(attrs.cx, "circle cx"),
        cy: number(attrs.cy, "circle cy"),
        r: number(attrs.r, "circle r"),
        fill,
      });
    } else {
      const points = attrs.points
        .trim()
        .split(/\s+/u)
        .map((point) => point.split(",").map(Number));
      if (points.length < 3 || points.some(([x, y]) => !Number.isFinite(x) || !Number.isFinite(y))) {
        throw new Error(`${relativePath}: invalid polygon points`);
      }
      shapes.push({ type, points, fill });
    }
  }

  const elementCount = [...text.matchAll(/<(rect|circle|polygon)\b/gu)].length;
  if (shapes.length === 0 || shapes.length !== elementCount) {
    throw new Error(`${relativePath}: unsupported or non-self-closing SVG geometry`);
  }
  return { width, height, shapes };
}

function contains(shape, x, y) {
  if (shape.type === "rect") {
    return x >= shape.x && x < shape.x + shape.width && y >= shape.y && y < shape.y + shape.height;
  }
  if (shape.type === "circle") {
    const dx = x - shape.cx;
    const dy = y - shape.cy;
    return dx * dx + dy * dy <= shape.r * shape.r;
  }

  let inside = false;
  for (let i = 0, j = shape.points.length - 1; i < shape.points.length; j = i, i += 1) {
    const [xi, yi] = shape.points[i];
    const [xj, yj] = shape.points[j];
    const crosses = yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi;
    if (crosses) inside = !inside;
  }
  return inside;
}

function render(svgs, width, height) {
  const canvasWidth = svgs[0].width;
  const canvasHeight = svgs[0].height;
  if (svgs.some((svg) => svg.width !== canvasWidth || svg.height !== canvasHeight)) {
    throw new Error("SVG layers must use the same canvas");
  }
  const shapes = svgs.flatMap((svg) => svg.shapes);
  const pixels = Buffer.alloc(width * height * 4);
  const samples = 2;

  for (let outputY = 0; outputY < height; outputY += 1) {
    for (let outputX = 0; outputX < width; outputX += 1) {
      const totals = [0, 0, 0, 0];
      for (let sampleY = 0; sampleY < samples; sampleY += 1) {
        for (let sampleX = 0; sampleX < samples; sampleX += 1) {
          const x = ((outputX + (sampleX + 0.5) / samples) / width) * canvasWidth;
          const y = ((outputY + (sampleY + 0.5) / samples) / height) * canvasHeight;
          for (let index = shapes.length - 1; index >= 0; index -= 1) {
            const shape = shapes[index];
            if (contains(shape, x, y)) {
              for (let channel = 0; channel < 4; channel += 1) totals[channel] += shape.fill[channel];
              break;
            }
          }
        }
      }
      const offset = (outputY * width + outputX) * 4;
      for (let channel = 0; channel < 4; channel += 1) {
        pixels[offset + channel] = Math.round(totals[channel] / (samples * samples));
      }
    }
  }
  return pixels;
}

const crcTable = Array.from({ length: 256 }, (_, index) => {
  let value = index;
  for (let bit = 0; bit < 8; bit += 1) {
    value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
  }
  return value >>> 0;
});

function crc32(data) {
  let crc = 0xffffffff;
  for (const byte of data) crc = crcTable[(crc ^ byte) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

function chunk(name, data) {
  const type = Buffer.from(name, "ascii");
  const result = Buffer.alloc(12 + data.length);
  result.writeUInt32BE(data.length, 0);
  type.copy(result, 4);
  data.copy(result, 8);
  result.writeUInt32BE(crc32(Buffer.concat([type, data])), 8 + data.length);
  return result;
}

function png(width, height, pixels) {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header[8] = 8;
  header[9] = 6;

  const scanlines = Buffer.alloc(height * (1 + width * 4));
  for (let y = 0; y < height; y += 1) {
    const destination = y * (1 + width * 4);
    scanlines[destination] = 0;
    pixels.copy(scanlines, destination + 1, y * width * 4, (y + 1) * width * 4);
  }

  const gamma = Buffer.alloc(4);
  gamma.writeUInt32BE(45455);
  return Buffer.concat([
    Buffer.from("89504e470d0a1a0a", "hex"),
    chunk("IHDR", header),
    chunk("sRGB", Buffer.from([0])),
    chunk("gAMA", gamma),
    chunk("IDAT", deflateSync(scanlines, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

function icns(images) {
  const parts = images.map(([type, data]) => {
    const part = Buffer.alloc(8 + data.length);
    part.write(type, 0, 4, "ascii");
    part.writeUInt32BE(part.length, 4);
    data.copy(part, 8);
    return part;
  });
  const body = Buffer.concat(parts);
  const header = Buffer.alloc(8);
  header.write("icns", 0, 4, "ascii");
  header.writeUInt32BE(8 + body.length, 4);
  return Buffer.concat([header, body]);
}

function ico(images) {
  const header = Buffer.alloc(6 + images.length * 16);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(images.length, 4);
  let offset = header.length;
  images.forEach(([size, data], index) => {
    const entry = 6 + index * 16;
    header[entry] = size === 256 ? 0 : size;
    header[entry + 1] = size === 256 ? 0 : size;
    header[entry + 2] = 0;
    header[entry + 3] = 0;
    header.writeUInt16LE(1, entry + 4);
    header.writeUInt16LE(32, entry + 6);
    header.writeUInt32LE(data.length, entry + 8);
    header.writeUInt32LE(offset, entry + 12);
    offset += data.length;
  });
  return Buffer.concat([header, ...images.map(([, data]) => data)]);
}

const appLayers = sourcePaths.slice(0, 2).map(parseSvg);
const appPngs = new Map();
for (const size of [32, 128, 256, 512, 1024]) {
  appPngs.set(size, png(size, size, render(appLayers, size, size)));
}

const generated = new Map([
  ["assets/branding/app-icon/app-icon-1024.png", appPngs.get(1024)],
  ["apps/desktop/src-tauri/icons/32x32.png", appPngs.get(32)],
  ["apps/desktop/src-tauri/icons/128x128.png", appPngs.get(128)],
  ["apps/desktop/src-tauri/icons/128x128@2x.png", appPngs.get(256)],
  [
    "apps/desktop/src-tauri/icons/icon.icns",
    icns([
      ["ic07", appPngs.get(128)],
      ["ic08", appPngs.get(256)],
      ["ic09", appPngs.get(512)],
      ["ic10", appPngs.get(1024)],
    ]),
  ],
  [
    "apps/desktop/src-tauri/icons/icon.ico",
    ico([
      [32, appPngs.get(32)],
      [128, appPngs.get(128)],
      [256, appPngs.get(256)],
    ]),
  ],
]);

for (const state of ["normal", "attention", "paused", "error"]) {
  const svg = parseSvg(`assets/branding/tray/tray-${state}.svg`);
  for (const [suffix, size] of [["", 18], ["@2x", 36]]) {
    generated.set(
      `apps/desktop/src-tauri/icons/tray/tray-${state}${suffix}.png`,
      png(size, size, render([svg], size, size)),
    );
  }
}

const sourceFiles = Object.fromEntries(
  sourcePaths.map((relativePath) => [relativePath, sha256(read(relativePath))]),
);
const sourceDigest = sha256(
  Buffer.concat(sourcePaths.flatMap((relativePath) => [Buffer.from(`${relativePath}\0`), read(relativePath)])),
);
const outputMetadata = [...generated.entries()].map(([relativePath, data]) => {
  const sizeMatch = relativePath.match(/(?:^|\/)(\d+)x\d+(@2x)?\.png$/u);
  const canonicalMatch = relativePath.match(/app-icon-(\d+)\.png$/u);
  const trayMatch = relativePath.match(/tray-[a-z]+(@2x)?\.png$/u);
  const dimensions = sizeMatch
    ? [Number(sizeMatch[1]) * (sizeMatch[2] ? 2 : 1), Number(sizeMatch[1]) * (sizeMatch[2] ? 2 : 1)]
    : canonicalMatch
      ? [Number(canonicalMatch[1]), Number(canonicalMatch[1])]
    : trayMatch
      ? [trayMatch[1] ? 36 : 18, trayMatch[1] ? 36 : 18]
      : null;
  return {
    path: relativePath,
    sha256: sha256(data),
    ...(dimensions ? { width: dimensions[0], height: dimensions[1] } : {}),
    role: relativePath.includes("/tray/") ? "macos-template" : "app-icon",
  };
});

for (const output of outputMetadata) {
  if (forbiddenFingerprints.sha256.includes(output.sha256)) {
    throw new Error(`${output.path}: matches a forbidden Tauri default icon fingerprint`);
  }
}

const manifest = {
  schema_version: 1,
  branding_status: "development-approved",
  release_approved: false,
  approval_scope: "development builds only",
  license: "MIT",
  provenance: "Original Aizu project artwork created 2026-08-12",
  canonical_canvas: { width: 1024, height: 1024 },
  color_profile: "sRGB",
  generator: {
    name: "scripts/generate-icons.mjs",
    version: GENERATOR_VERSION,
    sha256: sha256(read("scripts/generate-icons.mjs")),
    runtime: `node ${REQUIRED_NODE_VERSION.slice(1)}`,
    dependencies: "Node.js standard library only",
  },
  source_digest_sha256: sourceDigest,
  sources: sourceFiles,
  forbidden_default_icon_fingerprints: forbiddenFingerprints,
  outputs: outputMetadata,
};
const manifestBytes = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`);
generated.set("assets/branding/icon-manifest.json", manifestBytes);

let failed = false;
for (const [relativePath, expected] of generated) {
  const absolutePath = resolve(ROOT, relativePath);
  if (CHECK_MODE) {
    if (!existsSync(absolutePath)) {
      console.error(`missing generated asset: ${relativePath}`);
      failed = true;
    } else if (!readFileSync(absolutePath).equals(expected)) {
      console.error(`stale generated asset: ${relativePath}`);
      failed = true;
    }
  } else {
    mkdirSync(dirname(absolutePath), { recursive: true });
    writeFileSync(absolutePath, expected);
    console.log(`generated ${relativePath}`);
  }
}

if (failed) {
  console.error("run ./scripts/generate-icons.sh and commit the generated assets");
  process.exitCode = 1;
} else if (CHECK_MODE) {
  console.log(`validated ${generated.size - 1} generated assets and icon-manifest.json`);
}
