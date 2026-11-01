// The mazo explainer as a pure function of time: draw(ctx, t) paints the frame
// at t seconds on a W x H Canvas 2D context. It keeps no state between calls,
// so the browser preview and the frame renderer produce identical frames.

export const W = 1920;
export const H = 1080;
export const FPS = 30;
export const DURATION = 58;

const SANS = 'Inter, "Helvetica Neue", Arial, sans-serif';
const MONO = '"JetBrains Mono", "JetBrainsMono NFM", Menlo, Consolas, monospace';

const C = {
  bg: '#0b1017',
  panel: '#111821',
  panelEdge: '#1f2a36',
  text: '#e8eef5',
  dim: '#7d8896',
  faint: '#3a4552',
  cyan: '#5fd7e8',
  card1: '#0f8fb6',
  card2: '#12afd6',
  card3: '#18dcef',
  yellow: '#f2c75c',
  red: '#f07178',
  green: '#7ee0a1',
};

// ---------- timing helpers ----------

const clamp = (x, a = 0, b = 1) => Math.min(b, Math.max(a, x));
const lerp = (a, b, p) => a + (b - a) * p;
const prog = (t, a, b) => clamp((t - a) / (b - a));
const easeOut = (p) => 1 - Math.pow(1 - p, 3);
const easeInOut = (p) => (p < 0.5 ? 4 * p * p * p : 1 - Math.pow(-2 * p + 2, 3) / 2);
const easeBack = (p) => {
  const c = 1.6;
  return 1 + (c + 1) * Math.pow(p - 1, 3) + c * Math.pow(p - 1, 2);
};
// Opacity of a scene that lives in [a, b] with fades of length f at each end.
const window_ = (t, a, b, f = 0.45) => Math.min(prog(t, a, a + f), 1 - prog(t, b - f, b));

function rand(seed) {
  const x = Math.sin(seed * 127.1 + 311.7) * 43758.5453;
  return x - Math.floor(x);
}

// ---------- drawing helpers ----------

function rrect(ctx, x, y, w, h, r) {
  r = Math.min(r, w / 2, h / 2);
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
}

function text(ctx, s, x, y, { size = 40, weight = 400, font = SANS, color = C.text, align = 'left', alpha = 1, baseline = 'middle' } = {}) {
  if (alpha <= 0) return;
  ctx.save();
  ctx.globalAlpha *= alpha;
  ctx.font = `${weight} ${size}px ${font}`;
  ctx.fillStyle = color;
  ctx.textAlign = align;
  ctx.textBaseline = baseline;
  ctx.fillText(s, x, y);
  ctx.restore();
}

function measure(ctx, s, size, weight = 400, font = SANS) {
  ctx.save();
  ctx.font = `${weight} ${size}px ${font}`;
  const w = ctx.measureText(s).width;
  ctx.restore();
  return w;
}

// Text made of coloured runs: [[string, colour], ...].
function runs(ctx, parts, x, y, { size = 32, weight = 400, font = MONO, align = 'left', alpha = 1 } = {}) {
  const total = parts.reduce((w, [s]) => w + measure(ctx, s, size, weight, font), 0);
  let cx = align === 'center' ? x - total / 2 : align === 'right' ? x - total : x;
  for (const [s, color] of parts) {
    text(ctx, s, cx, y, { size, weight, font, color, alpha });
    cx += measure(ctx, s, size, weight, font);
  }
}

function headline(ctx, t, s, a, b, y = 150) {
  const o = window_(t, a, b);
  const rise = (1 - easeOut(prog(t, a, a + 0.6))) * 24;
  text(ctx, s, W / 2, y + rise, { size: 72, weight: 700, align: 'center', alpha: o });
}

function subline(ctx, t, s, a, b, y = 232, color = C.dim) {
  const o = window_(t, a, b);
  text(ctx, s, W / 2, y, { size: 40, weight: 500, align: 'center', color, alpha: o });
}

function panel(ctx, x, y, w, h, alpha = 1, title = null) {
  if (alpha <= 0) return;
  ctx.save();
  ctx.globalAlpha *= alpha;
  ctx.shadowColor = 'rgba(0,0,0,0.45)';
  ctx.shadowBlur = 40;
  ctx.shadowOffsetY = 16;
  rrect(ctx, x, y, w, h, 18);
  ctx.fillStyle = C.panel;
  ctx.fill();
  ctx.shadowColor = 'transparent';
  ctx.lineWidth = 2;
  ctx.strokeStyle = C.panelEdge;
  ctx.stroke();
  if (title) {
    for (let i = 0; i < 3; i++) {
      ctx.beginPath();
      ctx.arc(x + 30 + i * 22, y + 28, 6, 0, Math.PI * 2);
      ctx.fillStyle = C.faint;
      ctx.fill();
    }
    text(ctx, title, x + w / 2, y + 29, { size: 22, font: MONO, color: C.dim, align: 'center' });
  }
  ctx.restore();
}

// A prompt line that types itself out between a and a + dur.
function typed(ctx, t, cmd, x, y, a, dur = 0.9, size = 36, alpha = 1) {
  if (t < a || alpha <= 0) return;
  const n = Math.round(cmd.length * prog(t, a, a + dur));
  const shown = cmd.slice(0, n);
  runs(ctx, [['$ ', C.cyan], [shown, C.text]], x, y, { size, weight: 500, alpha });
  const blink = Math.floor(t * 2.5) % 2 === 0 || n < cmd.length;
  if (blink) {
    const cx = x + measure(ctx, '$ ' + shown, size, 500, MONO);
    ctx.save();
    ctx.globalAlpha *= alpha;
    ctx.fillStyle = C.cyan;
    ctx.fillRect(cx + 4, y - size * 0.55, size * 0.5, size * 1.1);
    ctx.restore();
  }
}

// A skill card: SKILL.md tag and a name, centred on (x, y).
function card(ctx, x, y, { name = '', w = 230, h = 300, rot = 0, color = C.card3, alpha = 1, scale = 1, glow = 0, label = 'SKILL.md' } = {}) {
  if (alpha <= 0) return;
  ctx.save();
  ctx.globalAlpha *= alpha;
  ctx.translate(x, y);
  ctx.rotate(rot);
  ctx.scale(scale, scale);
  ctx.shadowColor = glow > 0 ? `rgba(95,215,232,${0.6 * glow})` : 'rgba(0,0,0,0.5)';
  ctx.shadowBlur = glow > 0 ? 50 : 30;
  ctx.shadowOffsetY = glow > 0 ? 0 : 14;
  rrect(ctx, -w / 2, -h / 2, w, h, 24);
  ctx.fillStyle = color;
  ctx.fill();
  ctx.shadowColor = 'transparent';
  if (name) {
    text(ctx, label, -w / 2 + 24, -h / 2 + 36, { size: 20, font: MONO, weight: 700, color: 'rgba(6,24,32,0.65)' });
    for (let i = 0; i < 3; i++) {
      ctx.fillStyle = 'rgba(6,24,32,0.16)';
      rrect(ctx, -w / 2 + 24, -h / 2 + 70 + i * 26, (w - 48) * (i === 2 ? 0.55 : 0.9), 10, 5);
      ctx.fill();
    }
    const size = Math.min(28, ((w - 36) / name.length) * 1.6);
    text(ctx, name, 0, h / 2 - 50, { size, font: MONO, weight: 700, color: '#061820', align: 'center' });
  }
  ctx.restore();
}

// The mazo mark: three fanned cards, as in the logo.
function logoMark(ctx, x, y, s, alpha = 1, spread = 1) {
  const cards = [
    { dx: -70, dy: 10, rot: -0.52, color: C.card1 },
    { dx: -10, dy: -8, rot: -0.26, color: C.card2 },
    { dx: 50, dy: 0, rot: 0.3, color: C.card3 },
  ];
  for (const c of cards) {
    card(ctx, x + c.dx * s * spread, y + c.dy * s * spread, { w: 210 * s, h: 250 * s, rot: c.rot * spread, color: c.color, alpha });
  }
}

// The wordmark drawn as geometry, matching the logo's single-storey "a".
// size is comparable to a font size; the x-height is centred on y.
function wordmark(ctx, x, y, size, alpha = 1, align = 'left') {
  if (alpha <= 0) return;
  const h = size * 0.56;
  const sw = h * 0.24;
  const gap = h * 0.14;
  const widths = [h * 1.5, h, h * 0.86, h * 1.04];
  const total = widths.reduce((a, b) => a + b) + gap * 3;
  let cx = align === 'center' ? x - total / 2 : align === 'right' ? x - total : x;
  const top = y - h / 2 + size * 0.06;
  const bot = top + h;
  const T = top + sw / 2;
  ctx.save();
  ctx.globalAlpha *= alpha;
  ctx.strokeStyle = '#eef3f8';
  ctx.lineWidth = sw;
  ctx.lineCap = 'butt';
  ctx.lineJoin = 'miter';
  const ring = (ox, w) => {
    ctx.beginPath();
    ctx.ellipse(ox + w / 2, (top + bot) / 2, w / 2 - sw / 2, h / 2 - sw / 2, 0, 0, Math.PI * 2);
    ctx.stroke();
  };
  // m
  {
    const w = widths[0];
    const L = cx + sw / 2;
    const R = cx + w - sw / 2;
    const M = (L + R) / 2;
    const r = (M - L) / 2;
    ctx.beginPath();
    ctx.moveTo(L, bot);
    ctx.lineTo(L, top);
    ctx.moveTo(L, T + r);
    ctx.arc(L + r, T + r, r, Math.PI, 0);
    ctx.lineTo(M, bot);
    ctx.moveTo(M, T + r);
    ctx.arc(M + r, T + r, r, Math.PI, 0);
    ctx.lineTo(R, bot);
    ctx.stroke();
    cx += w + gap;
  }
  // a: a ring with a stem on the right
  {
    const w = widths[1];
    ring(cx, w);
    ctx.beginPath();
    ctx.moveTo(cx + w - sw / 2, top);
    ctx.lineTo(cx + w - sw / 2, bot);
    ctx.stroke();
    cx += w + gap;
  }
  // z
  {
    const w = widths[2];
    const B = bot - sw / 2;
    ctx.beginPath();
    ctx.moveTo(cx, T);
    ctx.lineTo(cx + w - sw * 0.7, T);
    ctx.lineTo(cx + sw * 0.7, B);
    ctx.lineTo(cx + w, B);
    ctx.stroke();
    cx += w + gap;
  }
  // o
  ring(cx, widths[3]);
  ctx.restore();
}

function cornerBrand(ctx, alpha) {
  if (alpha <= 0) return;
  logoMark(ctx, 92, 70, 0.17, alpha);
  wordmark(ctx, 128, 72, 34, alpha);
}

function background(ctx, t) {
  ctx.fillStyle = C.bg;
  ctx.fillRect(0, 0, W, H);
  const gx = W / 2 + Math.sin(t * 0.21) * 420;
  const gy = H / 2 + Math.cos(t * 0.17) * 160;
  const g = ctx.createRadialGradient(gx, gy, 0, gx, gy, 900);
  g.addColorStop(0, 'rgba(24,220,239,0.10)');
  g.addColorStop(1, 'rgba(24,220,239,0)');
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, W, H);
  ctx.fillStyle = 'rgba(255,255,255,0.045)';
  for (let x = 40; x < W; x += 48) {
    for (let y = 36; y < H; y += 48) ctx.fillRect(x, y, 2, 2);
  }
}

// ---------- scene 1-2: scattered skills become one deck ----------

const AGENTS = ['Claude Code', 'Codex', 'Pi'];
const AGENT_X = [480, 960, 1440];
const SKILLS = ['release-notes', 'triage', 'tidy-imports', 'unslop', 'grade', 'retro', 'commit-msg', 'pr-review', 'db-migrate'];

function scattered(i) {
  const col = i % 3;
  return {
    x: AGENT_X[col] + (rand(i + 1) - 0.5) * 200,
    y: 560 + Math.floor(i / 3) * 120 + (rand(i + 9) - 0.5) * 50,
    rot: (rand(i + 4) - 0.5) * 0.7,
  };
}

function sceneDeck(ctx, t) {
  if (t > 13.5) return;
  headline(ctx, t, 'Your skills are scattered across agents.', 0.2, 6.2);
  subline(ctx, t, 'Which ones actually get used?', 2.6, 6.2);
  headline(ctx, t, 'mazo makes them one collection.', 6.4, 9.6);

  const labelsAlpha = window_(t, 0.3, 6.6);
  AGENTS.forEach((a, i) => {
    text(ctx, a, AGENT_X[i], 360, { size: 40, weight: 600, align: 'center', color: C.cyan, alpha: labelsAlpha });
  });

  // Phase A: cards drop in. Phase B (6.4-7.6): they gather into the deck.
  // Phase C (7.6-8.6): the deck fans into the logo mark.
  const gather = easeInOut(prog(t, 6.4, 7.6));
  const toLogo = easeInOut(prog(t, 7.6, 8.6));
  const cardsAlpha = 1 - prog(t, 8.3, 8.7);
  SKILLS.forEach((name, i) => {
    const s = scattered(i);
    const drop = easeBack(prog(t, 0.4 + i * 0.22, 1.1 + i * 0.22));
    const sy = lerp(s.y - 150, s.y, drop);
    const dx = 960 + (i - 4) * 3;
    const dy = 600 - i * 4;
    const x = lerp(s.x, dx, gather);
    const y = lerp(sy, dy, gather);
    const rot = lerp(s.rot, (i - 4) * 0.02, gather);
    const colour = [C.card1, C.card2, C.card3][i % 3];
    card(ctx, x, y, { name, rot, color: colour, alpha: cardsAlpha * prog(t, 0.4 + i * 0.22, 0.75 + i * 0.22), scale: lerp(0.78, 0.9, gather) });
  });

  // Logo forms in the centre, then moves up for the sync step.
  const logoIn = prog(t, 8.2, 8.7);
  if (logoIn > 0) {
    const up = easeInOut(prog(t, 9.4, 10.2));
    const ly = lerp(560, 330, up);
    const ls = lerp(1, 0.62, up) * lerp(0.9, 1, toLogo);
    const out = 1 - prog(t, 12.9, 13.4);
    logoMark(ctx, 790 + (1 - up) * -40, ly, ls, logoIn * out, toLogo);
    wordmark(ctx, lerp(940, 900, up), ly + 8, lerp(190, 118, up), logoIn * out);
  }

  // Sync: the collection links into every agent home.
  const syncAlpha = window_(t, 9.8, 13.4);
  if (syncAlpha > 0) {
    headline(ctx, t, 'One collection, linked into every agent.', 9.8, 13.4);
    panel(ctx, 700, 460, 520, 84, syncAlpha);
    typed(ctx, t, 'mazo sync', 740, 502, 10.2, 0.6, 38, syncAlpha);
    AGENTS.forEach((a, i) => {
      const hx = AGENT_X[i];
      const hy = 820;
      const p = easeOut(prog(t, 10.9 + i * 0.25, 11.7 + i * 0.25));
      ctx.save();
      ctx.globalAlpha *= syncAlpha;
      ctx.strokeStyle = C.cyan;
      ctx.lineWidth = 4;
      ctx.setLineDash([14, 12]);
      ctx.lineDashOffset = -t * 40;
      ctx.beginPath();
      const sx = 960;
      const sy = 552;
      ctx.moveTo(sx, sy);
      ctx.lineTo(lerp(sx, hx, p), lerp(sy, hy - 60, p));
      ctx.stroke();
      ctx.restore();
      panel(ctx, hx - 190, hy - 60, 380, 130, syncAlpha);
      text(ctx, a, hx, hy - 8, { size: 40, weight: 600, align: 'center', alpha: syncAlpha });
      const ok = prog(t, 11.6 + i * 0.25, 11.9 + i * 0.25);
      text(ctx, '✓ linked', hx, hy + 38, { size: 28, font: MONO, weight: 700, color: C.green, align: 'center', alpha: syncAlpha * ok });
    });
  }
}

// ---------- scene 3-4: logs become usage; usage finds the unused ----------

const ROWS = [
  { name: 'release-notes', sess: 12, d7: 4, trend: '↑', last: '1d ago' },
  { name: 'unslop', sess: 7, d7: 3, trend: 'new', last: '2d ago' },
  { name: 'triage', sess: 5, d7: 1, trend: '→', last: '4d ago' },
  { name: 'tidy-imports', sess: 0, d7: 0, trend: '', last: '-' },
  { name: 'db-migrate', sess: 0, d7: 0, trend: '', last: '-' },
];

const LOG_LINES = [
  { s: '{"name":"Skill","input":{"skill":"release-notes"}}', row: 0, ch: 'tool' },
  { s: '/unslop tighten the changelog', row: 1, ch: 'slash' },
  { s: '$triage label the new issues', row: 2, ch: 'slash' },
  { s: '{"name":"Skill","input":{"skill":"release-notes"}}', row: 0, ch: 'tool' },
  { s: 'cat skills/unslop/SKILL.md', row: -1, ch: 'read' },
  { s: '/release-notes v0.4', row: 0, ch: 'slash' },
];

// Arrival times of each strong load, spread over [14.6, 21.6].
function arrivals(rowIdx) {
  const n = ROWS[rowIdx].sess;
  const out = [];
  for (let k = 0; k < n; k++) out.push(14.6 + ((k + 0.5) / n) * 7 + rowIdx * 0.13);
  return out;
}
const ARRIVALS = ROWS.map((_, i) => arrivals(i));

function sceneUsage(ctx, t) {
  if (t < 12.9 || t > 33.5) return;
  const o = window_(t, 13.4, 33.4);
  headline(ctx, t, 'It reads the logs your agents already write.', 13.45, 22.6);
  // Strong vs weak loads, shown once counting is under way.
  const legendA = window_(t, 18.6, 22.6);
  runs(ctx, [['strong: ', C.dim], ['slash · tool', C.cyan], ['     weak: ', C.dim], ['read · ref', C.yellow]], W / 2, 236, { size: 32, weight: 600, align: 'center', alpha: legendA });

  // The log panel slides out to the left when the unused scene starts.
  const logOut = easeInOut(prog(t, 22.4, 23.2));
  const lx = lerp(80, -800, logOut);
  panel(ctx, lx, 320, 720, 620, o, 'session logs');
  ctx.save();
  rrect(ctx, lx, 360, 720, 580, 18);
  ctx.clip();
  const scroll = (t - 13) * 70;
  for (let k = 0; k < 14; k++) {
    const idx = Math.floor(scroll / 64) + k;
    const line = LOG_LINES[((idx % LOG_LINES.length) + LOG_LINES.length) % LOG_LINES.length];
    const y = 900 - (k * 64 - (scroll % 64));
    if (y < 370 || y > 930) continue;
    const hot = line.row >= 0;
    text(ctx, line.s.length > 40 ? line.s.slice(0, 39) + '…' : line.s, lx + 30, y, { size: 24, font: MONO, color: hot ? C.text : C.dim, alpha: o });
    text(ctx, line.ch, lx + 690, y, { size: 22, font: MONO, weight: 700, align: 'right', color: hot ? C.cyan : C.yellow, alpha: o });
  }
  ctx.restore();

  // The usage table moves to the centre for the unused scene.
  const tx = lerp(840, 480, logOut);
  const ty = 330;
  const tw = 960;
  panel(ctx, tx, ty, tw, 600, o, 'mazo usage · sample');
  const cols = [
    { k: 'skill', x: 40, align: 'left' },
    { k: 'sess', x: 430, align: 'right' },
    { k: '7d', x: 530, align: 'right' },
    { k: 'trend', x: 680, align: 'right' },
    { k: 'last', x: 920, align: 'right' },
  ];
  cols.forEach((c) => text(ctx, c.k, tx + c.x, ty + 100, { size: 30, font: MONO, weight: 700, color: C.cyan, align: c.align, alpha: o }));

  const unusedP = prog(t, 24.6, 25.2);
  ROWS.forEach((r, i) => {
    const y = ty + 180 + i * 84;
    const arrived = ARRIVALS[i].filter((a) => t >= a).length;
    const d7 = Math.round((r.d7 * arrived) / Math.max(1, r.sess));
    const done = t >= 22.0;
    const isUnused = r.sess === 0;
    const retired = r.name === 'tidy-imports' ? prog(t, 28.6, 29.0) : 0;
    const rowA = o * (isUnused ? 1 : lerp(1, 0.35, unusedP)) * (1 - retired);
    if (isUnused && unusedP > 0) {
      ctx.save();
      ctx.globalAlpha *= o * unusedP * (1 - retired) * 0.18;
      ctx.fillStyle = C.red;
      rrect(ctx, tx + 20, y - 34, tw - 40, 68, 10);
      ctx.fill();
      ctx.restore();
    }
    const pulse = ARRIVALS[i].some((a) => t >= a && t < a + 0.25) ? 1 : 0;
    text(ctx, r.name, tx + 40, y, { size: 34, font: MONO, weight: 600, alpha: rowA });
    text(ctx, String(arrived), tx + 430, y, { size: 34, font: MONO, weight: 700, align: 'right', color: pulse ? C.cyan : C.text, alpha: rowA });
    text(ctx, String(d7), tx + 530, y, { size: 34, font: MONO, align: 'right', alpha: rowA });
    if (done) {
      const trendColor = r.trend === 'new' ? C.yellow : C.text;
      text(ctx, r.trend, tx + 680, y, { size: 34, font: MONO, align: 'right', color: trendColor, alpha: rowA * prog(t, 22, 22.4) });
    }
    const last = isUnused && unusedP > 0 ? 'unused' : arrived > 0 || done ? r.last : '-';
    text(ctx, last, tx + 920, y, { size: 34, font: MONO, weight: isUnused && unusedP > 0 ? 700 : 400, align: 'right', color: isUnused && unusedP > 0 ? C.red : C.dim, alpha: rowA });
  });

  // Particles: each strong load flies from the log panel to its row.
  if (logOut < 1) {
    ROWS.forEach((r, i) => {
      ARRIVALS[i].forEach((a) => {
        const p = prog(t, a - 0.7, a);
        if (p <= 0 || p >= 1) return;
        const e = easeInOut(p);
        const x0 = lx + 660;
        const y0 = 420 + rand(a) * 440;
        const x1 = tx + 400;
        const y1 = ty + 180 + i * 84;
        const x = lerp(x0, x1, e);
        const y = lerp(y0, y1, e) - Math.sin(e * Math.PI) * 90;
        ctx.save();
        ctx.globalAlpha *= o;
        ctx.shadowColor = C.cyan;
        ctx.shadowBlur = 20;
        ctx.fillStyle = C.cyan;
        ctx.beginPath();
        ctx.arc(x, y, 9, 0, Math.PI * 2);
        ctx.fill();
        ctx.restore();
      });
    });
  }

  // Unused, then retire.
  headline(ctx, t, 'Find the skills nobody uses.', 23.0, 28.2);
  panel(ctx, 420, 960, 700, 76, window_(t, 23.4, 33.4));
  typed(ctx, t, 'mazo unused', 450, 998, 23.8, 0.6, 34, window_(t, 23.4, 27.9, 0.2));
  typed(ctx, t, 'mazo retire tidy-imports', 450, 998, 27.9, 0.8, 34, window_(t, 27.9, 33.4, 0.2));
  headline(ctx, t, 'Retire what no longer earns its place.', 28.2, 33.4);

  // The retired row lifts off as a card and drops into .archive/.
  const archiveA = window_(t, 28.0, 33.4);
  if (archiveA > 0) {
    const bx = 1720;
    const by = 760;
    const fly = easeInOut(prog(t, 28.8, 30.2));
    const cx = lerp(tx + 300, bx, fly);
    const cy = lerp(ty + 180 + 3 * 84, by - 40, fly) - Math.sin(fly * Math.PI) * 160;
    card(ctx, cx, cy, { name: 'tidy-imports', w: 200, h: 260, rot: lerp(0, 0.2, fly), color: C.card1, alpha: archiveA * prog(t, 28.6, 28.9) * (1 - prog(t, 30.1, 30.4)), scale: lerp(0.7, 0.5, fly) });
    ctx.save();
    ctx.globalAlpha *= archiveA;
    rrect(ctx, bx - 150, by - 10, 300, 170, 16);
    ctx.fillStyle = C.panel;
    ctx.fill();
    ctx.lineWidth = 3;
    ctx.strokeStyle = prog(t, 30.1, 30.5) > 0 ? C.cyan : C.panelEdge;
    ctx.stroke();
    ctx.restore();
    text(ctx, '.archive/', bx, by + 60, { size: 36, font: MONO, weight: 700, align: 'center', alpha: archiveA });
    text(ctx, 'tidy-imports', bx, by + 112, { size: 26, font: MONO, color: C.dim, align: 'center', alpha: archiveA * prog(t, 30.2, 30.6) });
  }
}

// ---------- scene 5: audit against real sessions ----------

const IMPROVEMENTS = [
  'State the output format first',
  'Drop the duplicated checklist',
  'Show one good release note',
];

function sceneAudit(ctx, t) {
  if (t < 33 || t > 42.5) return;
  const o = window_(t, 33.45, 42.4);
  headline(ctx, t, 'Learn from real sessions.', 33.45, 42.4);
  subline(ctx, t, 'A judge you configure reviews how the skill actually ran.', 33.8, 42.4);
  panel(ctx, 560, 300, 800, 80, o);
  typed(ctx, t, 'mazo audit release-notes', 590, 340, 34.0, 0.9, 34, o);

  // Session transcripts flow into the judge.
  const jx = 760;
  const jy = 650;
  for (let k = 0; k < 6; k++) {
    const a = 35.0 + k * 0.45;
    const p = easeInOut(prog(t, a, a + 1.0));
    if (p >= 1) continue;
    const x = lerp(170, jx, p);
    const y = lerp(470 + k * 80, jy, p);
    const show = t >= a ? 1 : 0.35;
    ctx.save();
    ctx.globalAlpha *= o * show * (1 - p * 0.7);
    rrect(ctx, x - 110, y - 30, 220, 60, 12);
    ctx.fillStyle = C.panel;
    ctx.fill();
    ctx.strokeStyle = C.panelEdge;
    ctx.lineWidth = 2;
    ctx.stroke();
    ctx.restore();
    text(ctx, `session ${k + 1}`, x, y, { size: 24, font: MONO, align: 'center', color: C.dim, alpha: o * show * (1 - p * 0.7) });
  }
  const busy = prog(t, 35.0, 38.2) > 0 && t < 38.6;
  ctx.save();
  ctx.globalAlpha *= o;
  ctx.beginPath();
  ctx.arc(jx, jy, 110, 0, Math.PI * 2);
  ctx.fillStyle = C.panel;
  ctx.fill();
  ctx.lineWidth = 5;
  ctx.strokeStyle = C.cyan;
  ctx.stroke();
  if (busy) {
    ctx.beginPath();
    ctx.arc(jx, jy, 130, t * 4, t * 4 + 1.4);
    ctx.stroke();
  }
  ctx.restore();
  text(ctx, 'judge', jx, jy, { size: 40, weight: 700, align: 'center', alpha: o });

  // The report: rates mazo computes, and ranked improvements.
  const rx = 1000;
  const ry = 450;
  const repA = o * prog(t, 37.6, 38.2);
  panel(ctx, rx, ry, 820, 450, repA, 'skill report · sample');
  runs(ctx, [['followed ', C.dim], ['9 of 11', C.text], ['   achieved ', C.dim], ['8 of 11', C.text]], rx + 40, ry + 100, { size: 30, weight: 600, alpha: repA });
  text(ctx, 'ranked improvements', rx + 40, ry + 175, { size: 30, font: MONO, weight: 700, color: C.cyan, alpha: repA });
  IMPROVEMENTS.forEach((s, i) => {
    const a = prog(t, 38.4 + i * 0.5, 38.8 + i * 0.5);
    const slide = (1 - easeOut(a)) * 40;
    runs(ctx, [[`${i + 1}. `, C.yellow], [s, C.text]], rx + 40 + slide, ry + 250 + i * 64, { size: 32, font: SANS, weight: 500, alpha: repA * a });
  });
}

// ---------- scene 6: vetted install ----------

const VERDICT = [
  ['security', 'none found', C.green],
  ['quality', 'good', C.green],
  ['value', 'worth trying', C.green],
];

function sceneAdd(ctx, t) {
  if (t < 42 || t > 50.5) return;
  const o = window_(t, 42.45, 50.4);
  headline(ctx, t, 'Install from GitHub, vetted first.', 42.45, 50.4);
  subline(ctx, t, 'Untrusted sources go to your judge before anything is copied.', 42.8, 50.4);
  panel(ctx, 600, 300, 720, 80, o);
  typed(ctx, t, 'mazo add owner/repo', 630, 340, 43.0, 0.8, 34, o);

  const gx = 960;
  const gy = 660;
  // Gate.
  ctx.save();
  ctx.globalAlpha *= o;
  ctx.lineWidth = 6;
  ctx.strokeStyle = C.cyan;
  ctx.beginPath();
  ctx.moveTo(gx - 150, gy - 190);
  ctx.lineTo(gx - 150, gy + 190);
  ctx.moveTo(gx + 150, gy - 190);
  ctx.lineTo(gx + 150, gy + 190);
  ctx.stroke();
  ctx.restore();
  text(ctx, 'judge', gx, gy - 222, { size: 32, weight: 700, align: 'center', color: C.cyan, alpha: o });

  // The card travels in, is scanned, then joins the collection.
  const inP = easeInOut(prog(t, 43.8, 44.8));
  const outP = easeInOut(prog(t, 47.0, 48.0));
  const cx = outP > 0 ? lerp(gx, 1620, outP) : lerp(260, gx, inP);
  const scanning = t >= 44.8 && t < 47.0;
  card(ctx, cx, gy, { name: 'new-skill', w: 220, h: 290, color: C.card3, alpha: o * prog(t, 43.6, 43.9), glow: scanning ? 0.8 : 0 });
  if (scanning) {
    const sy = gy - 145 + ((t - 44.8) * 300) % 290;
    ctx.save();
    ctx.globalAlpha *= o;
    ctx.fillStyle = C.cyan;
    ctx.shadowColor = C.cyan;
    ctx.shadowBlur = 24;
    ctx.fillRect(gx - 140, sy, 280, 4);
    ctx.restore();
  }
  VERDICT.forEach(([k, v, col], i) => {
    const a = prog(t, 45.2 + i * 0.45, 45.6 + i * 0.45);
    runs(ctx, [[`${k}: `, C.dim], [v, col]], 230, 560 + i * 70, { size: 32, weight: 600, alpha: o * a * (1 - prog(t, 47.0, 47.4)) });
  });
  // Collection deck on the right, with the lock entry.
  logoMark(ctx, 1640, 640, 0.6, o * prog(t, 43.0, 43.6));
  runs(ctx, [['mazo.lock ', C.dim], ['+ verdict recorded', C.green]], 1640, 860, { size: 30, weight: 600, align: 'center', alpha: o * prog(t, 48.0, 48.4) });
}

// ---------- scene 7: outro ----------

function sceneOutro(ctx, t) {
  if (t < 50) return;
  const o = prog(t, 50.3, 51.0) * (1 - prog(t, 57.5, 58));
  const pop = easeBack(prog(t, 50.3, 51.1));
  logoMark(ctx, 700, 400, 0.85 * pop, o, pop);
  wordmark(ctx, 880, 410, 170, o);
  text(ctx, 'Make every skill earn its place.', W / 2, 640, { size: 68, weight: 700, align: 'center', alpha: o * prog(t, 51.0, 51.6) });
  runs(ctx, [['Claude Code', C.cyan], ['  ·  ', C.dim], ['Codex', C.cyan], ['  ·  ', C.dim], ['Pi', C.cyan]], W / 2, 740, { size: 40, font: SANS, weight: 600, align: 'center', alpha: o * prog(t, 51.8, 52.3) });
  text(ctx, 'Linux & macOS  ·  no telemetry  ·  MIT licensed', W / 2, 810, { size: 34, weight: 500, align: 'center', color: C.dim, alpha: o * prog(t, 52.2, 52.7) });
  text(ctx, 'github.com/nicolasgarcia214/mazo', W / 2, 920, { size: 40, font: MONO, weight: 700, align: 'center', color: C.text, alpha: o * prog(t, 52.8, 53.3) });
}

export function draw(ctx, t) {
  ctx.save();
  background(ctx, t);
  cornerBrand(ctx, window_(t, 13.0, 50.2));
  sceneDeck(ctx, t);
  sceneUsage(ctx, t);
  sceneAudit(ctx, t);
  sceneAdd(ctx, t);
  sceneOutro(ctx, t);
  // Opening fade from black.
  const fade = 1 - prog(t, 0, 0.4);
  if (fade > 0) {
    ctx.fillStyle = `rgba(11,16,23,${fade})`;
    ctx.fillRect(0, 0, W, H);
  }
  ctx.restore();
}
