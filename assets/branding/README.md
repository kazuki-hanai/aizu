# Aizu branding assets

This directory is the canonical source for Aizu application and menu bar
artwork. The current artwork is approved for development builds only. Release
approval is deliberately tracked separately in `icon-manifest.json`.

## Provenance and license

The Aizu-owned artwork under `app-icon/`, `tray/`, and `dmg/` was created for
the project on 2026-08-12. It contains no third-party trademarks and is
distributed under the repository's MIT license. Vendor marks under `agents/`
retain their owners' rights and are governed by `agents/README.md`.

The application icon uses an abstract notification aperture: a cyan signal
ring, light center, and coral status point on graphite. The menu bar artwork is
a separate black-and-transparent template glyph. Its four states preserve the
same base silhouette and add a shape-based status mark, so state is not encoded
by color.

## Source and generated files

- `app-icon/layers/*.svg` are the canonical 1024 x 1024 vector layers.
- `app-icon/icon-composer/README.md` records the Apple preview workflow.
- `tray/*.svg` are canonical 36 x 36 template-image sources.
- `dmg/background.svg` is the fixed Finder installer background and
  drag-to-Applications arrow.
- `previews/development-preview.png` shows the generated 128 px app icon and
  the 36 px menu bar template on simulated light, dark, and Increase Contrast
  surfaces, in that order from left to right.
- `app-icon/app-icon-1024.png` and `apps/desktop/src-tauri/icons/**` are
  generated package inputs. Do not edit them by hand.
- `icon-manifest.json` records Aizu-owned source, generator, and
  generated-output hashes. `agents/SHA256SUMS` records byte-exact vendor asset
  hashes, and the required check validates both inventories.

Regenerate and validate with the pinned repository toolchain:

```bash
./scripts/generate-icons.sh
./scripts/check-icons.sh
```

The generator uses only Node.js standard-library APIs. It emits deterministic
RGBA PNGs with explicit sRGB and gamma chunks, an ICNS containing PNG icon
representations, and an ICO containing PNG representations.

Before release, a project owner must set `branding_status` to `approved` and
`release_approved` to `true` through the approval process, regenerate the
manifest, and attach Finder/Dock plus light/dark/Increase Contrast previews to
the release PR. Development approval must not be presented as release approval.
