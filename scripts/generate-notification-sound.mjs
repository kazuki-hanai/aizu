#!/usr/bin/env node

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const sampleRate = 44_100;
const durationSeconds = 0.44;
const sampleCount = Math.round(sampleRate * durationSeconds);
const channels = 1;
const bitsPerSample = 16;
const bytesPerSample = bitsPerSample / 8;
const output = resolve(dirname(fileURLToPath(import.meta.url)), "../assets/audio/aizu-pop.wav");

const clamp = (value, minimum, maximum) => Math.min(maximum, Math.max(minimum, value));
const smoothstep = (value) => {
  const x = clamp(value, 0, 1);
  return x * x * (3 - 2 * x);
};

let noiseState = 0x41_49_5a_55;
const noise = () => {
  noiseState = (Math.imul(noiseState, 1_664_525) + 1_013_904_223) >>> 0;
  return noiseState / 0xffff_ffff * 2 - 1;
};

const tone = (time, start, duration, frequency, gain, phaseOffset) => {
  const local = time - start;
  if (local < 0 || local >= duration) return 0;

  const attack = smoothstep(local / 0.006);
  const release = smoothstep((duration - local) / 0.075);
  const body = Math.exp(-local * 3.5);
  const envelope = attack * release * body;
  const glide = 1 - 0.11 * Math.exp(-local * 34);
  const phase = Math.PI * 2 * frequency * (local + 0.11 / 34 * (Math.exp(-local * 34) - 1));
  const fundamental = Math.sin(phase + phaseOffset);
  const sparkle = Math.sin(phase * 2.01 + 0.45) * 0.22 + Math.sin(phase * 3.98 + 1.1) * 0.08;
  const transient = noise() * Math.exp(-local * 92) * 0.075;
  return (fundamental + sparkle + transient) * envelope * gain * glide;
};

const floats = new Float64Array(sampleCount);
for (let index = 0; index < sampleCount; index += 1) {
  const time = index / sampleRate;
  const first = tone(time, 0.012, 0.255, 880, 0.62, 0);
  const second = tone(time, 0.145, 0.285, 1_108.73, 0.78, 0.16);
  const warmth = tone(time, 0.147, 0.27, 554.365, 0.12, 0.5);
  const masterFade = smoothstep((durationSeconds - time) / 0.025);
  floats[index] = (first + second + warmth) * masterFade;
}

let maximum = 0;
for (const sample of floats) maximum = Math.max(maximum, Math.abs(sample));
const targetPeak = 10 ** (-4 / 20);
const scale = maximum === 0 ? 0 : targetPeak / maximum;

const dataSize = sampleCount * channels * bytesPerSample;
const buffer = Buffer.alloc(44 + dataSize);
buffer.write("RIFF", 0, "ascii");
buffer.writeUInt32LE(36 + dataSize, 4);
buffer.write("WAVE", 8, "ascii");
buffer.write("fmt ", 12, "ascii");
buffer.writeUInt32LE(16, 16);
buffer.writeUInt16LE(1, 20);
buffer.writeUInt16LE(channels, 22);
buffer.writeUInt32LE(sampleRate, 24);
buffer.writeUInt32LE(sampleRate * channels * bytesPerSample, 28);
buffer.writeUInt16LE(channels * bytesPerSample, 32);
buffer.writeUInt16LE(bitsPerSample, 34);
buffer.write("data", 36, "ascii");
buffer.writeUInt32LE(dataSize, 40);

for (let index = 0; index < sampleCount; index += 1) {
  const sample = clamp(floats[index] * scale, -1, 1);
  buffer.writeInt16LE(Math.round(sample * 32_767), 44 + index * bytesPerSample);
}

const check = process.argv.includes("--check");
const expectedHeader = {
  channels,
  sampleRate,
  bitsPerSample,
  sampleCount,
};
if (check) {
  let current;
  try {
    current = readFileSync(output);
  } catch {
    process.stderr.write(`missing generated notification sound: ${output}\n`);
    process.exit(1);
  }
  if (!current.equals(buffer)) {
    process.stderr.write("generated notification sound is stale; run scripts/generate-notification-sound.mjs\n");
    process.exit(1);
  }
  if (current.toString("ascii", 0, 4) !== "RIFF"
    || current.toString("ascii", 8, 12) !== "WAVE"
    || current.readUInt16LE(22) !== expectedHeader.channels
    || current.readUInt32LE(24) !== expectedHeader.sampleRate
    || current.readUInt16LE(34) !== expectedHeader.bitsPerSample
    || current.readUInt32LE(40) / bytesPerSample !== expectedHeader.sampleCount) {
    process.stderr.write("generated notification sound has an invalid PCM contract\n");
    process.exit(1);
  }
  let measuredPeak = 0;
  for (let offset = 44; offset < current.length; offset += bytesPerSample) {
    measuredPeak = Math.max(measuredPeak, Math.abs(current.readInt16LE(offset)) / 32_767);
  }
  const measuredDb = 20 * Math.log10(measuredPeak);
  if (Math.abs(measuredDb - -4) > 0.02) {
    process.stderr.write(`generated notification sound peak is ${measuredDb.toFixed(2)} dBFS; expected -4.00 dBFS\n`);
    process.exit(1);
  }
} else {
  writeFileSync(output, buffer);
}

process.stdout.write(`validated Aizu Pop: ${String(sampleCount)} samples, ${String(sampleRate)} Hz, -4 dBFS peak\n`);
