# Icon Composer preview notes

`../layers/background.svg` and `../layers/foreground.svg` are deliberately
separate so they can be imported as background and foreground layers in Apple
Icon Composer. The source contains no rounded platform mask, baked shadow,
gloss, or appearance-specific tint.

For release approval, preview the layers using the current Xcode-pinned Icon
Composer in default, dark, clear, and tinted appearances. Record the Xcode and
Icon Composer versions plus screenshots in the release PR. Icon Composer output
is not used by the deterministic development generator.
