#!/usr/bin/env node

import { existsSync, readFileSync, statSync } from "node:fs";
import { dirname, extname, resolve } from "node:path";

const ROOT = resolve(import.meta.dirname, "..");
const markdownPaths = [
  "README.md",
  "AGENTS.md",
  ".github/PULL_REQUEST_TEMPLATE.md",
  "assets/branding/README.md",
  "assets/branding/app-icon/icon-composer/README.md",
  "docs/mvp-design.md",
  "docs/protocol.md",
];
const schemaPath = "docs/schemas/event-v1.schema.json";
const errors = [];

function read(relativePath) {
  return readFileSync(resolve(ROOT, relativePath), "utf8");
}

function slugify(heading) {
  return heading
    .trim()
    .toLowerCase()
    .replace(/[`*_~]/gu, "")
    .replace(/[^\p{L}\p{N}\s-]/gu, "")
    .replace(/\s+/gu, "-")
    .replace(/-+/gu, "-");
}

const anchorsByPath = new Map();
for (const relativePath of markdownPaths) {
  const anchors = new Set();
  const duplicateCounts = new Map();
  for (const line of read(relativePath).split("\n")) {
    const match = line.match(/^#{1,6}\s+(.+?)\s*$/u);
    if (!match) continue;
    const base = slugify(match[1]);
    const count = duplicateCounts.get(base) ?? 0;
    duplicateCounts.set(base, count + 1);
    anchors.add(count === 0 ? base : `${base}-${count}`);
  }
  anchorsByPath.set(relativePath, anchors);
}

for (const relativePath of markdownPaths) {
  const text = read(relativePath);
  const fenceCount = [...text.matchAll(/^```/gmu)].length;
  if (fenceCount % 2 !== 0) errors.push(`${relativePath}: unclosed fenced code block`);

  for (const match of text.matchAll(/(?<!!)\[[^\]]+\]\(([^)]+)\)/gu)) {
    let target = match[1].trim();
    if (target.startsWith("<") && target.endsWith(">")) target = target.slice(1, -1);
    if (/^(?:https?:|mailto:)/u.test(target)) continue;
    const [pathPart, anchor] = target.split("#", 2);
    const resolvedPath = pathPart
      ? resolve(ROOT, dirname(relativePath), decodeURIComponent(pathPart))
      : resolve(ROOT, relativePath);
    if (!existsSync(resolvedPath)) {
      errors.push(`${relativePath}: broken link ${target}`);
      continue;
    }
    if (statSync(resolvedPath).isDirectory() || !anchor || extname(resolvedPath) !== ".md") continue;
    const linkedPath = resolvedPath.slice(ROOT.length + 1);
    const anchors = anchorsByPath.get(linkedPath);
    if (anchors && !anchors.has(decodeURIComponent(anchor).toLowerCase())) {
      errors.push(`${relativePath}: missing anchor ${target}`);
    }
  }
}

let schema;
try {
  schema = JSON.parse(read(schemaPath));
} catch (error) {
  errors.push(`${schemaPath}: invalid JSON: ${error.message}`);
}

function inspectSchema(node, location = "$", seen = new Set()) {
  if (!node || typeof node !== "object" || Array.isArray(node) || seen.has(node)) return;
  seen.add(node);
  const validTypes = new Set(["null", "boolean", "object", "array", "number", "string", "integer"]);
  if (node.type && !validTypes.has(node.type)) errors.push(`${schemaPath}:${location}: unknown type ${node.type}`);
  if (node.pattern) {
    try {
      new RegExp(node.pattern, "u");
    } catch (error) {
      errors.push(`${schemaPath}:${location}: invalid pattern: ${error.message}`);
    }
  }
  if (node.required) {
    if (!Array.isArray(node.required) || new Set(node.required).size !== node.required.length) {
      errors.push(`${schemaPath}:${location}: required must be a unique array`);
    }
    for (const key of node.required) {
      if (!node.properties?.[key] && location === "$.") {
        errors.push(`${schemaPath}:${location}: required property ${key} is not declared`);
      }
    }
  }
  if (node.enum && (!Array.isArray(node.enum) || new Set(node.enum.map(JSON.stringify)).size !== node.enum.length)) {
    errors.push(`${schemaPath}:${location}: enum must contain unique values`);
  }
  for (const [key, value] of Object.entries(node)) inspectSchema(value, `${location}.${key}`, seen);
}

function validateEvent(event, location) {
  if (!event || typeof event !== "object" || Array.isArray(event)) {
    errors.push(`${location}: event must be an object`);
    return;
  }
  for (const key of schema.required) {
    if (!(key in event)) errors.push(`${location}: missing required field ${key}`);
  }
  for (const [key, value] of Object.entries(event)) {
    const property = schema.properties[key];
    if (!property) continue;
    if (property.const !== undefined && value !== property.const) errors.push(`${location}.${key}: const mismatch`);
    if (property.enum && !property.enum.includes(value)) errors.push(`${location}.${key}: value is not in enum`);
    if (property.type === "string" && typeof value !== "string") errors.push(`${location}.${key}: expected string`);
    if (property.type === "integer" && !Number.isInteger(value)) errors.push(`${location}.${key}: expected integer`);
    if (typeof value === "string") {
      if (property.minLength !== undefined && [...value].length < property.minLength) errors.push(`${location}.${key}: too short`);
      if (property.maxLength !== undefined && [...value].length > property.maxLength) errors.push(`${location}.${key}: too long`);
      if (property.pattern && !new RegExp(property.pattern, "u").test(value)) errors.push(`${location}.${key}: pattern mismatch`);
    }
  }
  if (event.kind === "task.completed" && !("outcome" in event)) errors.push(`${location}: task.completed needs outcome`);
  if (event.kind === "agent.question" && "outcome" in event) errors.push(`${location}: agent.question must not have outcome`);
  if (event.source && typeof event.source === "object") {
    for (const key of schema.properties.source.required) {
      if (!(key in event.source)) errors.push(`${location}.source: missing required field ${key}`);
    }
  }
  if (Buffer.byteLength(JSON.stringify(event), "utf8") > 65_536) errors.push(`${location}: exceeds 65536 bytes`);
}

if (schema) {
  if (schema.$schema !== "https://json-schema.org/draft/2020-12/schema") {
    errors.push(`${schemaPath}: must use JSON Schema draft 2020-12`);
  }
  if (schema.$id !== "urn:aizu:schema:event:v1" || schema.properties?.schema_version?.const !== 1) {
    errors.push(`${schemaPath}: event v1 identity/version mismatch`);
  }
  inspectSchema(schema, "$");
}

for (const relativePath of ["docs/mvp-design.md", "docs/protocol.md"]) {
  const text = read(relativePath);
  let blockIndex = 0;
  for (const match of text.matchAll(/```json\s*\n([\s\S]*?)\n```/gu)) {
    blockIndex += 1;
    const block = match[1].trim();
    let records;
    try {
      records = [{ line: block, value: JSON.parse(block), lineIndex: 0 }];
    } catch {
      records = block.split("\n").filter(Boolean).map((line, lineIndex) => {
        try {
          return { line, value: JSON.parse(line), lineIndex };
        } catch (error) {
          errors.push(`${relativePath}:json-block-${blockIndex}:${lineIndex + 1}: invalid NDJSON: ${error.message}`);
          return { line, value: null, lineIndex };
        }
      });
    }
    records.forEach(({ line, value }) => {
      if (value === null) return;
      if (value.schema_version !== undefined) validateEvent(value, `${relativePath}:json-block-${blockIndex}`);
      if (value.type === "event") validateEvent(value.event, `${relativePath}:json-block-${blockIndex}.event`);
      if (Buffer.byteLength(line, "utf8") > 131_072) errors.push(`${relativePath}:json-block-${blockIndex}: frame exceeds 131072 bytes`);
    });
  }
}

const design = read("docs/mvp-design.md");
const protocol = read("docs/protocol.md");
const schemaText = read(schemaPath);
for (const [name, text] of [["design", design], ["protocol", protocol], ["schema", schemaText]]) {
  if (!text.includes("64 KiB") && !text.includes("65536")) errors.push(`${name}: missing 64 KiB event limit`);
}
if (!design.includes("128 KiB") || !protocol.includes("131072 bytes")) {
  errors.push("design/protocol: 128 KiB frame limit is inconsistent or missing");
}

if (errors.length > 0) {
  for (const error of errors) console.error(error);
  process.exitCode = 1;
} else {
  console.log("validated Markdown links/fences, schema contract, JSON examples, and size limits");
}
