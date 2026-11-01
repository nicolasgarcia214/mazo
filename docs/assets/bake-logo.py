#!/usr/bin/env python3
"""Bake logo-dark.png into src/tui/logo_art.rs: the logo resized with Lanczos
to the TUI's pixel grid, two pixels to a terminal cell. Requires ImageMagick.
Run from any directory: python docs/assets/bake-logo.py.

The dark background becomes no pixel, so the terminal's own background shows.
The near-white wordmark becomes ink, the terminal's foreground, so it reads on
a light theme too.
"""

from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "docs/assets/logo-dark.png"
TARGET = ROOT / "src/tui/logo_art.rs"
# The content of logo-dark.png, its background trimmed.
CROP = "1975x526+101+109"
# Cells across. A cell is about twice as tall as wide, so a pixel row is half a
# cell and the pixels come out square.
COLS = 56
# Restores the edges the downscale softens, short of haloing them.
SHARPEN = "0x0.6+1.0"
# A pixel no channel of which reaches this is background.
DARK = 48


def pixels() -> list[list[tuple[int, int, int]]]:
    """CROP of the logo resized to COLS pixels across and the even number of
    rows that keeps its aspect."""
    w, h = (int(n) for n in CROP.split("+")[0].split("x"))
    rows = round(h * COLS / w / 2) * 2
    raw = subprocess.run(
        ["magick", str(SOURCE), "-crop", CROP, "+repage", "-filter", "Lanczos",
         "-resize", f"{COLS}x{rows}!", "-unsharp", SHARPEN, "-depth", "8", "rgb:-"],
        capture_output=True, check=True).stdout
    flat = [tuple(raw[i:i + 3]) for i in range(0, len(raw), 3)]
    return [flat[r * COLS:(r + 1) * COLS] for r in range(rows)]


def code(p: tuple[int, int, int]) -> str:
    """`p` in Rust: `0` for the background, `INK` for the near-white
    wordmark, else `0xRRGGBB`."""
    if max(p) < DARK:
        return "0"
    if min(p) > 200 and max(p) - min(p) < 24:
        return "INK"
    return "0x%02x%02x%02x" % p


def main():
    grid = pixels()
    rows = "\n".join("    [" + ", ".join(map(code, row)) + "]," for row in grid)
    TARGET.write_text(
        "//! The logo's pixels, baked from `docs/assets/logo-dark.png` by\n"
        "//! `docs/assets/bake-logo.py`; rerun it rather than editing here.\n\n"
        "/// The wordmark's ink: the terminal's foreground.\n"
        "pub const INK: u32 = 1;\n\n"
        "/// The deck of cards beside the wordmark, a row of pixels to a row,\n"
        "/// two rows to a cell: `0` for none, [`INK`], or `0xRRGGBB`.\n"
        "#[rustfmt::skip]\n"
        f"pub const LOGO: [[u32; {COLS}]; {len(grid)}] = [\n{rows}\n];\n")
    print(f"Baked {TARGET}: {COLS}×{len(grid) // 2} cells")


if __name__ == "__main__":
    main()
