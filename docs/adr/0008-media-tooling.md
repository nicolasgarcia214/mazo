# 0008. Media and docs tooling

Status: Accepted (2026-11-26)

## Context
The scripts that bake the TUI logo, capture TUI screenshots and render the explainer video recorded their design choices as code comments. The comment ban (ADR 0001) removes those comments; this record keeps the decisions.

## Decisions
### Logo baking (`docs/assets/bake-logo.py`)
- `CROP` is the geometry of `logo-dark.png`'s content with its background trimmed, so the baked art carries no margin.
- `COLS` sets the width in terminal cells, and each pixel row maps to half a cell. A cell is about twice as tall as it is wide, so the pixels render square.
- `SHARPEN` restores the edges the downscale softens, kept below the strength that would put halos on them.
- `DARK` is the per-channel threshold: a pixel with no channel reaching it counts as background.

### TUI capture (`docs/assets/capture-tui.py`)
- Block elements (`█`, `▀`, `▄`, `▍`) are drawn as rects filling their share of the cell, as a terminal draws them, because rendering them as font glyphs leaves the font's gaps.

### Explainer video (`media/explainer`)
- `draw(ctx, t)` in `scene.js` is a pure function of time that keeps no state between calls, so the browser preview and the frame renderer produce identical frames.
- `render.mjs` renders every frame of `scene.js` and pipes raw RGBA into ffmpeg, which encodes an H.264 `yuv420p` MP4 with `+faststart` so YouTube and X accept it.
- `wordmark` draws the letters as geometry rather than with a font, so the "a" matches the logo's single-storey form.
