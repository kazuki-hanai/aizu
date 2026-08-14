# Aizu notification audio

The four canonical Aizu notification sounds are generated deterministically by
`scripts/generate-notification-sound.mjs` and are covered by the repository
license:

- `aizu-pop.wav`: bright 440 ms rising two-note cue
- `aizu-chime.wav`: clear 820 ms descending bell sequence
- `aizu-pulse.wav`: rounded 580 ms low two-pulse cue
- `aizu-bloom.wav`: warm 760 ms expanding chord

Every asset is 44.1 kHz, 16-bit mono and normalized to a -4 dBFS peak. Do not
edit the WAV files directly. Regenerate and verify them with:

```bash
mise exec -- node scripts/generate-notification-sound.mjs
mise exec -- node scripts/generate-notification-sound.mjs --check
```
