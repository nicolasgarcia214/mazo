#!/usr/bin/env python3
"""Capture the real TUI with synthetic test data. Requires cargo, rsvg-convert,
and the JetBrains Mono font. Run from any directory: python docs/assets/capture-tui.py.
The temporary integration test is removed even if capture fails.
"""

import html
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
COLS, ROWS = 140, 25
CELL_W, CELL_H = 9, 22
PAD, TOP = 32, 72
BG, FG = "#101722", "#dce5ef"
PALETTE = {
    "Reset": FG, "Black": BG, "Red": "#f08d8d", "Green": "#a6d995",
    "Yellow": "#f3cc7a", "Blue": "#8fb9ef", "Magenta": "#c4a1e8",
    "Cyan": "#67d9e8", "Gray": "#cbd5e1", "DarkGray": "#8290a3",
    "LightRed": "#ffaaaa", "LightGreen": "#bde7aa", "LightYellow": "#ffe3a0",
    "LightBlue": "#a5cfff", "LightMagenta": "#dbb9ff", "LightCyan": "#a5f3fc",
    "White": "#ffffff", "Indexed(236)": "#303030",
}
RUST = r'''
mod common;
use ratatui::{Terminal, backend::TestBackend, style::Modifier};
#[test]
fn capture() {
    let mut golden = common::golden::Golden::new();
    let bundled = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("skills");
    for skill in ["grade", "retro"] {
        common::copy_tree(&bundled.join(skill), &golden.repo.path().join(skill), &|s| s);
    }
    golden.world.inv = mazo::inventory::Inventory::load(golden.repo.path()).unwrap();
    let input = golden.world.input(&golden.events);
    let mut app = mazo::tui::App::new(&input, std::path::Path::new("/nonexistent"));
    let mut terminal = Terminal::new(TestBackend::new(140, 25)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let cells: Vec<_> = (0..25).flat_map(|y| (0..140).map(move |x| (x, y)))
        .map(|(x, y)| {
            let cell = &buffer[(x, y)];
            serde_json::json!({"x": x, "y": y, "text": cell.symbol(),
                "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg),
                "bold": cell.modifier.contains(Modifier::BOLD)})
        }).collect();
    let text = cells.iter().map(|c| c["text"].as_str().unwrap()).collect::<String>();
    assert!(text.contains("writing-for-agents") && text.contains("Removes slop from prose."));
    assert!(!text.contains("not installed"));
    let path = std::env::var("MAZO_CAPTURE_JSON").unwrap();
    std::fs::write(path, serde_json::to_vec(&cells).unwrap()).unwrap();
}
'''


def colour(name):
    """The hex of a ratatui colour's Debug name: a named one, or `Rgb(r, g, b)`."""
    if name.startswith("Rgb("):
        return "#%02x%02x%02x" % tuple(int(n) for n in name[4:-1].split(","))
    return PALETTE[name]


def capture():
    with tempfile.TemporaryDirectory(prefix="mazo-readme-") as scratch:
        data = Path(scratch) / "cells.json"
        with tempfile.NamedTemporaryFile(mode="w", prefix="readme_capture_", suffix=".rs",
                                         dir=ROOT / "tests", delete=False) as harness:
            harness.write(RUST)
            source = Path(harness.name)
        try:
            subprocess.run(["cargo", "test", "--locked", "--offline", "--quiet", "--test", source.stem],
                           cwd=ROOT, env={**os.environ, "MAZO_CAPTURE_JSON": str(data)}, check=True)
        finally:
            source.unlink(missing_ok=True)
        cells = json.loads(data.read_text())
        width, height = COLS * CELL_W + 2 * PAD, ROWS * CELL_H + TOP + PAD
        svg = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
               f'viewBox="0 0 {width} {height}">',
               '<title>Mazo usage table and skill card — synthetic sample data</title>',
               f'<rect width="{width}" height="{height}" rx="16" fill="{BG}"/>',
               f'<path d="M0 53H{width}" stroke="#273243"/>',
               '<g font-family="sans-serif" font-size="13">',
               '<text x="32" y="33" fill="#a8b6c8">mazo · sample workspace</text>',
               f'<text x="{width - PAD}" y="33" text-anchor="end" '
               'fill="#8290a3" font-size="11" letter-spacing="1.1">SYNTHETIC SAMPLE DATA</text></g>']
        for cell in cells:
            x, y = PAD + cell["x"] * CELL_W, TOP + cell["y"] * CELL_H
            if cell["bg"] != "Reset":
                svg.append(f'<rect x="{x}" y="{y}" width="{CELL_W}" height="{CELL_H}" '
                           f'fill="{colour(cell["bg"])}"/>')
        svg.append('<g font-family="JetBrains Mono, JetBrainsMono Nerd Font Mono, monospace" font-size="15">')
        # A block element fills its share of the cell, as a terminal draws it;
        # as text it would leave the font's gaps.
        blocks = {"█": (0, 0, 1, 1), "▀": (0, 0, 1, .5), "▄": (0, .5, 1, .5), "▍": (0, 0, 3 / 8, 1)}
        for cell in cells:
            if cell["text"] in blocks:
                bx, by, bw, bh = blocks[cell["text"]]
                x, y = PAD + cell["x"] * CELL_W, TOP + cell["y"] * CELL_H
                svg.append(f'<rect x="{x + bx * CELL_W}" y="{y + by * CELL_H}" width="{bw * CELL_W}" '
                           f'height="{bh * CELL_H}" fill="{colour(cell["fg"])}"/>')
        for cell in cells:
            if not cell["text"].strip() or cell["text"] in blocks:
                continue
            x, y = PAD + cell["x"] * CELL_W, TOP + cell["y"] * CELL_H + 16
            weight = ' font-weight="700"' if cell["bold"] else ""
            svg.append(f'<text x="{x}" y="{y}" fill="{colour(cell["fg"])}"{weight}>'
                       f'{html.escape(cell["text"])}</text>')
        svg.append('</g></svg>')
        vector = Path(scratch) / "tui.svg"
        vector.write_text("\n".join(svg))
        target = Path(__file__).with_name("tui.png")
        subprocess.run(["rsvg-convert", "--output", str(target), str(vector)], check=True)
        print(f"Captured App::draw: {COLS}×{ROWS} cells, {width}×{height} px → {target}")


if __name__ == "__main__":
    capture()
