// Renders every frame of scene.js and pipes raw RGBA into ffmpeg, which
// encodes an MP4 that YouTube and X accept. Usage: node render.mjs [out.mp4]
import { spawn } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { createCanvas, GlobalFonts } from '@napi-rs/canvas';
import { draw, W, H, FPS, DURATION } from './scene.js';

const FONTS = [
  ['/usr/share/fonts/TTF/JetBrainsMonoNerdFontMono-Regular.ttf', 'JetBrains Mono'],
  ['/usr/share/fonts/TTF/JetBrainsMonoNerdFontMono-Bold.ttf', 'JetBrains Mono'],
  [`${process.env.HOME}/.local/share/fonts/Inter-variable.ttf`, 'Inter'],
];
for (const [path, family] of FONTS) {
  if (!GlobalFonts.registerFromPath(path, family)) throw new Error(`cannot load font ${path}`);
}

const out = resolve(process.argv[2] ?? 'out/mazo-explainer.mp4');
mkdirSync(dirname(out), { recursive: true });

const ffmpeg = spawn('ffmpeg', [
  '-y', '-loglevel', 'error',
  '-f', 'rawvideo', '-pix_fmt', 'rgba', '-s', `${W}x${H}`, '-r', String(FPS), '-i', '-',
  '-f', 'lavfi', '-i', 'anullsrc=channel_layout=stereo:sample_rate=48000',
  '-map', '0:v', '-map', '1:a', '-shortest',
  '-c:v', 'libx264', '-profile:v', 'high', '-preset', 'slow', '-crf', '18',
  '-vf', 'setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709', '-pix_fmt', 'yuv420p', '-g', String(FPS * 2),
  '-c:a', 'aac', '-b:a', '128k',
  '-movflags', '+faststart',
  out,
], { stdio: ['pipe', 'inherit', 'inherit'] });
const exited = new Promise((ok, fail) => ffmpeg.on('close', (code) => (code === 0 ? ok() : fail(new Error(`ffmpeg exited ${code}`)))));

const canvas = createCanvas(W, H);
const ctx = canvas.getContext('2d');
const frames = Math.round(DURATION * FPS);
for (let f = 0; f < frames; f++) {
  ctx.clearRect(0, 0, W, H);
  draw(ctx, f / FPS);
  const rgba = ctx.getImageData(0, 0, W, H).data;
  if (!ffmpeg.stdin.write(Buffer.from(rgba.buffer, rgba.byteOffset, rgba.byteLength))) {
    await new Promise((ok) => ffmpeg.stdin.once('drain', ok));
  }
  if (f % FPS === 0) process.stderr.write(`\r${f / FPS}s / ${DURATION}s`);
}
ffmpeg.stdin.end();
await exited;
process.stderr.write(`\nwrote ${out}\n`);
