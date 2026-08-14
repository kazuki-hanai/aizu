#!/usr/bin/env node

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const sampleRate = 44_100;
const channels = 1;
const bitsPerSample = 16;
const bytesPerSample = bitsPerSample / 8;
const targetPeakDb = -4;
const targetPeak = 10 ** (targetPeakDb / 20);
const audioDirectory = resolve(dirname(fileURLToPath(import.meta.url)), "../assets/audio");

const clamp = (value, minimum, maximum) => Math.min(maximum, Math.max(minimum, value));
const smoothstep = (value) => {
  const x = clamp(value, 0, 1);
  return x * x * (3 - 2 * x);
};

const createNoise = (seed) => {
  let state = seed >>> 0;
  return () => {
    state = (Math.imul(state, 1_664_525) + 1_013_904_223) >>> 0;
    return state / 0xffff_ffff * 2 - 1;
  };
};

const renderSamples = (durationSeconds, render) => {
  const sampleCount = Math.round(sampleRate * durationSeconds);
  const samples = new Float64Array(sampleCount);
  for (let index = 0; index < sampleCount; index += 1) {
    samples[index] = render(index / sampleRate);
  }
  return samples;
};

const popProfile = () => {
  const durationSeconds = 0.44;
  const noise = createNoise(0x41_49_5a_55);
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

  return renderSamples(durationSeconds, (time) => {
    const first = tone(time, 0.012, 0.255, 880, 0.62, 0);
    const second = tone(time, 0.145, 0.285, 1_108.73, 0.78, 0.16);
    const warmth = tone(time, 0.147, 0.27, 554.365, 0.12, 0.5);
    const masterFade = smoothstep((durationSeconds - time) / 0.025);
    return (first + second + warmth) * masterFade;
  });
};

const chimeProfile = () => {
  const durationSeconds = 0.82;
  const noise = createNoise(0x43_48_49_4d);
  const bell = (time, start, frequency, gain) => {
    const local = time - start;
    if (local < 0) return 0;
    const attack = smoothstep(local / 0.004);
    const decay = Math.exp(-local * 5.2);
    const tail = smoothstep((durationSeconds - time) / 0.06);
    const phase = Math.PI * 2 * frequency * local;
    const partials = Math.sin(phase)
      + Math.sin(phase * 2.013 + 0.35) * 0.34
      + Math.sin(phase * 3.97 + 1.2) * 0.14
      + Math.sin(phase * 6.11 + 0.8) * 0.06;
    const strike = noise() * Math.exp(-local * 135) * 0.045;
    return (partials + strike) * attack * decay * tail * gain;
  };

  return renderSamples(durationSeconds, (time) =>
    bell(time, 0.012, 1_318.51, 0.54)
      + bell(time, 0.19, 987.767, 0.72)
      + bell(time, 0.37, 659.255, 0.32));
};

const pulseProfile = () => {
  const durationSeconds = 0.58;
  const noise = createNoise(0x50_55_4c_53);
  const pulse = (time, start, frequency, gain) => {
    const local = time - start;
    if (local < 0 || local >= 0.28) return 0;
    const attack = smoothstep(local / 0.008);
    const release = smoothstep((0.28 - local) / 0.08);
    const decay = Math.exp(-local * 7.5);
    const drop = frequency * (1 + 0.14 * Math.exp(-local * 24));
    const phase = Math.PI * 2 * drop * local;
    const body = Math.sin(phase) + Math.sin(phase * 2 + 0.25) * 0.18;
    const click = noise() * Math.exp(-local * 115) * 0.055;
    return (body + click) * attack * release * decay * gain;
  };

  return renderSamples(durationSeconds, (time) => {
    const first = pulse(time, 0.018, 246.942, 0.8);
    const second = pulse(time, 0.265, 329.628, 0.94);
    const overtone = pulse(time, 0.27, 659.255, 0.12);
    const masterFade = smoothstep((durationSeconds - time) / 0.035);
    return (first + second + overtone) * masterFade;
  });
};

const bloomProfile = () => {
  const durationSeconds = 0.76;
  const voice = (time, start, frequency, gain, phaseOffset) => {
    const local = time - start;
    if (local < 0) return 0;
    const attack = smoothstep(local / 0.09);
    const release = smoothstep((durationSeconds - time) / 0.18);
    const shimmer = 1 + Math.sin(local * Math.PI * 2 * 3.1 + phaseOffset) * 0.0025;
    const phase = Math.PI * 2 * frequency * shimmer * local + phaseOffset;
    const body = Math.sin(phase) + Math.sin(phase * 2 + 0.4) * 0.09;
    return body * attack * release * Math.exp(-local * 1.35) * gain;
  };

  return renderSamples(durationSeconds, (time) => {
    const root = voice(time, 0.012, 523.251, 0.38, 0);
    const third = voice(time, 0.028, 659.255, 0.35, 0.7);
    const fifth = voice(time, 0.044, 783.991, 0.32, 1.1);
    const octave = voice(time, 0.17, 1_046.5, 0.12, 0.25);
    return root + third + fifth + octave;
  });
};

const profiles = [
  { name: "Aizu Pop", fileName: "aizu-pop.wav", render: popProfile },
  { name: "Aizu Chime", fileName: "aizu-chime.wav", render: chimeProfile },
  { name: "Aizu Pulse", fileName: "aizu-pulse.wav", render: pulseProfile },
  { name: "Aizu Bloom", fileName: "aizu-bloom.wav", render: bloomProfile },
];

const encodePcm = (samples) => {
  let maximum = 0;
  for (const sample of samples) maximum = Math.max(maximum, Math.abs(sample));
  const scale = maximum === 0 ? 0 : targetPeak / maximum;
  const dataSize = samples.length * channels * bytesPerSample;
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

  for (let index = 0; index < samples.length; index += 1) {
    const sample = clamp(samples[index] * scale, -1, 1);
    buffer.writeInt16LE(Math.round(sample * 32_767), 44 + index * bytesPerSample);
  }
  return buffer;
};

const validatePcm = (profile, buffer, sampleCount) => {
  if (buffer.toString("ascii", 0, 4) !== "RIFF"
    || buffer.toString("ascii", 8, 12) !== "WAVE"
    || buffer.readUInt16LE(22) !== channels
    || buffer.readUInt32LE(24) !== sampleRate
    || buffer.readUInt16LE(34) !== bitsPerSample
    || buffer.readUInt32LE(40) / bytesPerSample !== sampleCount) {
    throw new Error(`${profile.name} has an invalid PCM contract`);
  }
  let measuredPeak = 0;
  for (let offset = 44; offset < buffer.length; offset += bytesPerSample) {
    measuredPeak = Math.max(measuredPeak, Math.abs(buffer.readInt16LE(offset)) / 32_767);
  }
  const measuredDb = 20 * Math.log10(measuredPeak);
  if (Math.abs(measuredDb - targetPeakDb) > 0.02) {
    throw new Error(`${profile.name} peak is ${measuredDb.toFixed(2)} dBFS; expected ${targetPeakDb.toFixed(2)} dBFS`);
  }
};

const check = process.argv.includes("--check");
for (const profile of profiles) {
  const samples = profile.render();
  const generated = encodePcm(samples);
  const output = resolve(audioDirectory, profile.fileName);
  validatePcm(profile, generated, samples.length);
  if (check) {
    let current;
    try {
      current = readFileSync(output);
    } catch {
      process.stderr.write(`missing generated notification sound: ${output}\n`);
      process.exitCode = 1;
      continue;
    }
    if (!current.equals(generated)) {
      process.stderr.write(`${profile.name} is stale; run scripts/generate-notification-sound.mjs\n`);
      process.exitCode = 1;
      continue;
    }
    try {
      validatePcm(profile, current, samples.length);
    } catch (error) {
      process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
      process.exitCode = 1;
      continue;
    }
  } else {
    writeFileSync(output, generated);
  }
  const durationMs = Math.round(samples.length / sampleRate * 1_000);
  process.stdout.write(`validated ${profile.name}: ${String(durationMs)} ms, ${String(sampleRate)} Hz, ${String(targetPeakDb)} dBFS peak\n`);
}
