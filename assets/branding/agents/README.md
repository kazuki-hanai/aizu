# Official agent brand assets

These files identify third-party agents in Aizu. They are vendor-provided
artwork, not Aizu artwork, and are not covered by Aizu's MIT license.

All SVG and PNG files in this directory are byte-for-byte copies from the
official archives described below. Aizu must not recolor, distort, crop, trace,
or combine them with the Aizu mark.

## Codex / OpenAI

Use `openai/OAI_OpenAI-Blossom_Black.svg` on a light surface and
`openai/OAI_OpenAI-Blossom_White.svg` on a dark surface as a small provider
identifier next to the visible label **Codex**.

OpenAI does not publish a Codex-specific logo in its current logo archive or
in the official `openai/codex` repository. The Blossom therefore identifies
OpenAI, not Codex itself. Do not call it a Codex logo, use it without a `Codex`
text label, or present it as Aizu's primary branding.

- Brand page: <https://openai.com/brand/>
- Logo archive exposed by the page after accepting the Marks usage terms:
  <https://cdn.openai.com/brand/openai-logos.zip>
- Retrieved: 2026-08-13
- Archive SHA-256:
  `c54e85ab5884228f89f0230dd8effa8d588cad78166fe954135f4afa553222db`
- Official Codex repository checked for a product-specific mark:
  <https://github.com/openai/codex>

`OpenAI-Partnership-Templates-2025.zip`, also linked from the brand page,
contains two Photoshop partnership templates and no standalone logo asset. It
is therefore not a source for these files.

OpenAI's brand terms require the mark to relate directly to an OpenAI service,
be used exactly as provided, and remain less prominent than Aizu's own name or
mark. They prohibit implied endorsement and using the Blossom as primary
branding. Permission is non-exclusive, non-transferable, may be updated, and
may be terminated by OpenAI. Recheck the current terms before a release:
<https://openai.com/brand/#usage-terms>.

## Claude Code / Anthropic

Use `anthropic/ClaudeIcon-Rounded.svg` for a compact agent row or notification
source indicator. Use the official `Claude Code logo - *.svg` wordmark only
where the full lockup has adequate room. Choose the supplied Slate, Ivory, or
One-color original appropriate to the surface; do not recolor it.

- Press kit landing URL: <https://www.anthropic.com/press-kit>
- Official archive URL returned by that page on retrieval:
  <https://www-cdn.anthropic.com/ae59ca4ca194dac9c9dc3bc78c5829468cb0e8af.zip>
- Retrieved: 2026-08-13
- Archive SHA-256:
  `c68ac92df86c825f95177e24016fcc9a8863a3fd4ca344fe6f0700b2c1e07151`

The press kit explicitly names the product artwork `Claude Code logo`, so it
is preferred over the generic Anthropic corporate symbol for Claude Code.
Anthropic's download does not include a permissive software or artwork license.
Anthropic and Claude trademarks remain Anthropic's property. Use these files
only for accurate product identification, without modification or implied
endorsement, and obtain legal/brand approval if a distribution context requires
rights beyond nominative identification.

## Integrity

Verify the copied vendor files from the repository root:

```bash
shasum -a 256 -c assets/branding/agents/SHA256SUMS
```
