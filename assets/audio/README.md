# Aizu notification audio

`aizu-pop.wav` is the canonical Aizu notification sound. It is generated
deterministically by `scripts/generate-notification-sound.mjs` and is covered
by the repository license.

The sound is a 44.1 kHz, 16-bit mono, approximately 440 ms two-note cue. Its
peak is normalized to -4 dBFS. Do not edit the WAV directly. Regenerate and
verify it with:

```bash
mise exec -- node scripts/generate-notification-sound.mjs
mise exec -- node scripts/generate-notification-sound.mjs --check
```
