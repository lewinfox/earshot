// Scrolling colour waveform. Its shape is always the real signal: for every pixel
// column, the lowest and highest sample in that slice of time, from a 2000-per-second
// min/max summary (with a 1/16 copy for when zoomed out). Zoomed in, that traces the
// individual cycles. The colour comes from the band summary (150 per second): bass red,
// mids green, highs blue, like Rekordbox's RGB waveform. Both are uploaded once as
// textures and a fragment shader draws the visible window each frame, so per-frame cost
// does not depend on track length. Beats, cues, loops and the playhead go on a 2D
// canvas on top (only a few dozen lines per frame).

import type { GridDto } from "./types";

const TEX_W = 4096;
/** Detail bins per bin of the coarse min/max copy. */
const MIP = 16;
/** Most detail bins the shader reads per pixel before switching to the coarse copy. */
const MAX_FINE = 24;
/** Below this many detail bins per pixel the wave is drawn as a thin line. */
const LINE_BELOW = "3.0";
/** Half the line's width, in device pixels. */
const LINE_HALF_WIDTH = "0.6";
// Bass carries most of the energy in dance music; these lift mids and highs so they
// still show through.
const MID_GAIN = "2.0";
const HIGH_GAIN = "4.0";

const VS = `#version 300 es
in vec2 p;
void main() { gl_Position = vec4(p, 0.0, 1.0); }`;

const FS = `#version 300 es
precision highp float;
uniform sampler2D uBands;  // RGBA per 1/150 s: peak, low, mid, high
uniform sampler2D uFine;   // RG per 1/2000 s: min, max (128 = 0)
uniform sampler2D uMip;    // RG per ${MIP} fine bins: min, max
uniform float uT;          // playhead, seconds
uniform float uSecPerPx;
uniform vec2 uSize;        // canvas size in device pixels
uniform float uBandRate;
uniform float uBandBins;
uniform float uFineRate;
uniform float uFineBins;
out vec4 color;

ivec2 at(float i) { return ivec2(int(mod(i, ${TEX_W}.0)), int(i / ${TEX_W}.0)); }
vec2 minmax(sampler2D s, float i) { return (texelFetch(s, at(i), 0).rg * 255.0 - 128.0) / 127.0; }

// Bands are stored square-rooted; squaring again and then cubing the ratios to the
// strongest band keeps the colours saturated, so a kick reads red and a hat blue.
vec3 bandRGB(vec3 b) {
  b = b * b * vec3(1.0, ${MID_GAIN}, ${HIGH_GAIN});
  vec3 c = b / max(max(b.r, b.g), max(b.b, 1e-4));
  return 0.12 + 0.88 * c * c * c;
}

void main() {
  vec2 frag = gl_FragCoord.xy;
  vec3 bg = vec3(0.03, 0.035, 0.045);
  float t0 = uT + (frag.x - 0.5 - uSize.x * 0.5) * uSecPerPx;
  float t1 = t0 + uSecPerPx;
  float a = max(0.0, floor(t0 * uFineRate));
  // The last bin that starts inside this pixel, so neighbouring columns don't overlap.
  float b = min(uFineBins - 1.0, max(a, ceil(t1 * uFineRate) - 1.0));
  if (b < a) { color = vec4(bg, 1.0); return; }

  // Lowest and highest sample in this pixel's slice of time.
  float lo = 1.0, hi = -1.0;
  if (b - a < ${MAX_FINE}.0) {
    for (int k = 0; k < ${MAX_FINE}; k++) {
      float i = a + float(k);
      if (i > b) break;
      vec2 v = minmax(uFine, i);
      lo = min(lo, v.x); hi = max(hi, v.y);
    }
  } else {
    float ma = floor(a / ${MIP}.0), mb = floor(b / ${MIP}.0);
    for (int k = 0; k < 256; k++) {
      float i = ma + float(k);
      if (i > mb) break;
      vec2 v = minmax(uMip, i);
      lo = min(lo, v.x); hi = max(hi, v.y);
    }
  }

  float px = 2.0 / uSize.y;
  float y = (frag.y - uSize.y * 0.5) / (uSize.y * 0.5);
  if (hi - lo < px) { float m = 0.5 * (lo + hi); lo = m - 0.5 * px; hi = m + 0.5 * px; }
  // The full range, with softened edges.
  float cover = clamp((y - lo) / px + 0.5, 0.0, 1.0) * clamp((hi - y) / px + 0.5, 0.0, 1.0);

  // Zoomed in far enough to see cycles: a thin line through the middle of each bin,
  // with the full range kept as a faint shadow so high frequencies still show.
  float binsPerPx = uSecPerPx * uFineRate;
  if (binsPerPx < ${LINE_BELOW}) {
    float halfH = uSize.y * 0.5;
    float tf = uT + (frag.x - uSize.x * 0.5) * uSecPerPx;   // time at this pixel's centre
    float first = floor((tf - 2.0 * uSecPerPx) * uFineRate - 0.5);
    float dist = 1e9;
    vec2 prev = vec2(0.0);
    for (int k = 0; k < 16; k++) {
      float i = first + float(k);
      if (i < 0.0 || i >= uFineBins) continue;
      vec2 v = minmax(uFine, i);
      float tc = (i + 0.5) / uFineRate;
      vec2 p = vec2(uSize.x * 0.5 + (tc - uT) / uSecPerPx, halfH + 0.5 * (v.x + v.y) * halfH);
      if (k > 0 && i > 0.0) {
        vec2 d = p - prev, q = frag - prev;
        float h = clamp(dot(q, d) / max(dot(d, d), 1e-6), 0.0, 1.0);
        dist = min(dist, length(q - d * h));
      }
      prev = p;
      if (p.x > frag.x + 2.0) break;
    }
    cover = max(0.25 * cover, clamp(${LINE_HALF_WIDTH} + 0.5 - dist, 0.0, 1.0));
  }
  if (cover <= 0.0) { color = vec4(bg, 1.0); return; }

  // Colour from the bands around the middle of the pixel.
  float f = clamp(0.5 * (t0 + t1) * uBandRate - 0.5, 0.0, uBandBins - 1.0);
  vec4 w = mix(texelFetch(uBands, at(floor(f)), 0), texelFetch(uBands, at(min(floor(f) + 1.0, uBandBins - 1.0)), 0), fract(f));
  vec3 c = bandRGB(w.yzw);
  float played = frag.x < uSize.x * 0.5 ? 0.55 : 1.0;
  color = vec4(mix(bg, c * played, cover), 1.0);
}`;

/** Same colours as the shader, for the overview: bass red, mids green, highs blue. */
export function bandColour(low: number, mid: number, high: number): string {
  const b = [low * low, mid * mid * +MID_GAIN, high * high * +HIGH_GAIN];
  const m = Math.max(...b, 1e-4);
  const ch = (x: number) => Math.round(255 * (0.12 + 0.88 * (x / m) ** 3));
  return `rgb(${ch(b[0])},${ch(b[1])},${ch(b[2])})`;
}

function compile(gl: WebGL2RenderingContext, type: number, src: string) {
  const s = gl.createShader(type)!;
  gl.shaderSource(s, src);
  gl.compileShader(s);
  if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s) ?? "shader");
  return s;
}

export interface WaveFrame {
  position: number;
  /** Seconds visible either side of the playhead. */
  halfWindow: number;
  grid: GridDto | null;
  cuePoint: number;
  hotcues: (number | null)[];
  loopIn: number | null;
  loopOut: number | null;
  loopActive: boolean;
}

export const HOTCUE_COLOURS = ["#3cd36b", "#ff4f4f", "#ffb02e", "#2f9bff", "#c25bff", "#18d2c7", "#ff74c8", "#e6e04a"];

export class WaveformView {
  private gl: WebGL2RenderingContext;
  private overlay: CanvasRenderingContext2D;
  private prog: WebGLProgram;
  private bandsTex: WebGLTexture;
  private fineTex: WebGLTexture;
  private mipTex: WebGLTexture;
  private u: Record<string, WebGLUniformLocation | null> = {};
  private bandBins = 0;
  private bandRate = 150;
  private fineBins = 0;
  private fineRate = 2000;

  constructor(
    private glCanvas: HTMLCanvasElement,
    private overlayCanvas: HTMLCanvasElement,
  ) {
    const gl = glCanvas.getContext("webgl2", { antialias: false, alpha: false });
    if (!gl) throw new Error("WebGL2 is not available");
    this.gl = gl;
    this.overlay = overlayCanvas.getContext("2d")!;
    const prog = gl.createProgram()!;
    gl.attachShader(prog, compile(gl, gl.VERTEX_SHADER, VS));
    gl.attachShader(prog, compile(gl, gl.FRAGMENT_SHADER, FS));
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog) ?? "link");
    this.prog = prog;
    for (const n of ["uBands", "uFine", "uMip", "uT", "uSecPerPx", "uSize", "uBandRate", "uBandBins", "uFineRate", "uFineBins"]) {
      this.u[n] = gl.getUniformLocation(prog, n);
    }
    const buf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
    const loc = gl.getAttribLocation(prog, "p");
    gl.enableVertexAttribArray(loc);
    gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
    this.bandsTex = gl.createTexture()!;
    this.fineTex = gl.createTexture()!;
    this.mipTex = gl.createTexture()!;
  }

  /** `bands`: 4 bytes per bin (peak, low, mid, high). `fine`: 2 bytes per bin (min, max). */
  setWaveform(bands: Uint8Array, bandRate: number, fine: Uint8Array, fineRate: number) {
    this.bandBins = bands.length / 4;
    this.bandRate = bandRate;
    this.fineBins = fine.length / 2;
    this.fineRate = fineRate;
    this.upload(this.bandsTex, bands, 4);
    this.upload(this.fineTex, fine, 2);
    // Coarse min/max copy: each bin covers MIP fine bins.
    const n = Math.ceil(this.fineBins / MIP);
    const mip = new Uint8Array(n * 2);
    for (let j = 0; j < n; j++) {
      let lo = 255;
      let hi = 0;
      for (let i = j * MIP; i < Math.min((j + 1) * MIP, this.fineBins); i++) {
        lo = Math.min(lo, fine[i * 2]);
        hi = Math.max(hi, fine[i * 2 + 1]);
      }
      mip[j * 2] = lo;
      mip[j * 2 + 1] = hi;
    }
    this.upload(this.mipTex, mip, 2);
  }

  private upload(tex: WebGLTexture, data: Uint8Array, channels: 2 | 4) {
    const gl = this.gl;
    const bins = data.length / channels;
    const rows = Math.max(1, Math.ceil(bins / TEX_W));
    const padded = new Uint8Array(TEX_W * rows * channels);
    padded.set(data);
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    if (channels === 4) gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, TEX_W, rows, 0, gl.RGBA, gl.UNSIGNED_BYTE, padded);
    else gl.texImage2D(gl.TEXTURE_2D, 0, gl.RG8, TEX_W, rows, 0, gl.RG, gl.UNSIGNED_BYTE, padded);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
  }

  private fit(c: HTMLCanvasElement) {
    const dpr = window.devicePixelRatio || 1;
    const w = Math.round(c.clientWidth * dpr);
    const h = Math.round(c.clientHeight * dpr);
    if (c.width !== w || c.height !== h) {
      c.width = w;
      c.height = h;
    }
    return [w, h];
  }

  draw(f: WaveFrame) {
    const gl = this.gl;
    const [w, h] = this.fit(this.glCanvas);
    gl.viewport(0, 0, w, h);
    gl.useProgram(this.prog);
    [this.bandsTex, this.fineTex, this.mipTex].forEach((t, i) => {
      gl.activeTexture(gl.TEXTURE0 + i);
      gl.bindTexture(gl.TEXTURE_2D, t);
    });
    gl.uniform1i(this.u.uBands, 0);
    gl.uniform1i(this.u.uFine, 1);
    gl.uniform1i(this.u.uMip, 2);
    gl.uniform1f(this.u.uT, f.position);
    gl.uniform1f(this.u.uSecPerPx, (2 * f.halfWindow) / w);
    gl.uniform2f(this.u.uSize, w, h);
    gl.uniform1f(this.u.uBandRate, this.bandRate);
    gl.uniform1f(this.u.uBandBins, this.bandBins);
    gl.uniform1f(this.u.uFineRate, this.fineRate);
    gl.uniform1f(this.u.uFineBins, this.fineBins);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    this.drawOverlay(f);
  }

  private drawOverlay(f: WaveFrame) {
    const [w, h] = this.fit(this.overlayCanvas);
    const ctx = this.overlay;
    const dpr = window.devicePixelRatio || 1;
    ctx.clearRect(0, 0, w, h);
    const x = (t: number) => w / 2 + ((t - f.position) / f.halfWindow) * (w / 2);
    const t0 = f.position - f.halfWindow;
    const t1 = f.position + f.halfWindow;

    if (f.loopIn != null && f.loopOut != null) {
      ctx.fillStyle = f.loopActive ? "rgba(255,180,40,0.22)" : "rgba(255,255,255,0.08)";
      ctx.fillRect(x(f.loopIn), 0, x(f.loopOut) - x(f.loopIn), h);
    }
    if (f.grid) {
      const times = f.grid.times;
      let i = lowerBound(times, t0);
      for (; i < times.length && times[i] <= t1; i++) {
        const one = f.grid.positions[i] === 1;
        ctx.fillStyle = one ? "rgba(255,70,70,0.95)" : "rgba(255,255,255,0.35)";
        const lw = (one ? 2 : 1) * dpr;
        ctx.fillRect(Math.round(x(times[i]) - lw / 2), 0, lw, one ? h : h * 0.18);
        if (!one) ctx.fillRect(Math.round(x(times[i]) - lw / 2), h * 0.82, lw, h * 0.18);
      }
    }
    const marker = (t: number, colour: string, label: string) => {
      if (t < t0 || t > t1) return;
      const px = x(t);
      ctx.fillStyle = colour;
      ctx.fillRect(px - dpr, 0, 2 * dpr, h);
      ctx.beginPath();
      ctx.moveTo(px - 6 * dpr, 0);
      ctx.lineTo(px + 6 * dpr, 0);
      ctx.lineTo(px, 8 * dpr);
      ctx.fill();
      if (label) {
        ctx.font = `${10 * dpr}px system-ui`;
        ctx.fillText(label, px + 4 * dpr, h - 4 * dpr);
      }
    };
    marker(f.cuePoint, "#ff8c28", "");
    f.hotcues.forEach((t, i) => t != null && marker(t, HOTCUE_COLOURS[i], "ABCDEFGH"[i]));
    ctx.fillStyle = "#ffffff";
    ctx.fillRect(Math.round(w / 2 - dpr), 0, 2 * dpr, h);
  }
}

export function lowerBound(a: number[], v: number) {
  let lo = 0;
  let hi = a.length;
  while (lo < hi) {
    const m = (lo + hi) >> 1;
    if (a[m] < v) lo = m + 1;
    else hi = m;
  }
  return lo;
}

/** Whole-track overview, drawn once per load; the playhead is drawn on top per frame. */
export class OverviewView {
  private base: HTMLCanvasElement | null = null;
  private ctx: CanvasRenderingContext2D;

  constructor(private canvas: HTMLCanvasElement) {
    this.ctx = canvas.getContext("2d")!;
  }

  setWaveform(data: Uint8Array) {
    const dpr = window.devicePixelRatio || 1;
    const w = Math.max(1, Math.round(this.canvas.clientWidth * dpr));
    const h = Math.max(1, Math.round(this.canvas.clientHeight * dpr));
    const base = document.createElement("canvas");
    base.width = w;
    base.height = h;
    const c = base.getContext("2d")!;
    const bins = data.length / 4;
    for (let x = 0; x < w; x++) {
      const a = Math.floor((x / w) * bins);
      const b = Math.max(a + 1, Math.floor(((x + 1) / w) * bins));
      let peak = 0;
      let lo = 0;
      let mid = 0;
      let hi = 0;
      for (let i = a; i < b; i++) {
        peak = Math.max(peak, data[i * 4]);
        lo += data[i * 4 + 1];
        mid += data[i * 4 + 2];
        hi += data[i * 4 + 3];
      }
      const amp = (peak / 255) * (h / 2);
      c.fillStyle = bandColour(lo, mid, hi);
      c.fillRect(x, h / 2 - amp, 1, amp * 2);
    }
    this.base = base;
  }

  draw(
    position: number,
    duration: number,
    loopIn: number | null,
    loopOut: number | null,
    cuePoint: number,
    hotcues: (number | null)[],
  ) {
    const dpr = window.devicePixelRatio || 1;
    const w = Math.round(this.canvas.clientWidth * dpr);
    const h = Math.round(this.canvas.clientHeight * dpr);
    if (this.canvas.width !== w || this.canvas.height !== h) {
      this.canvas.width = w;
      this.canvas.height = h;
    }
    const ctx = this.ctx;
    ctx.clearRect(0, 0, w, h);
    if (this.base) ctx.drawImage(this.base, 0, 0, w, h);
    if (!duration) return;
    if (loopIn != null && loopOut != null) {
      ctx.fillStyle = "rgba(255,180,40,0.35)";
      ctx.fillRect((loopIn / duration) * w, 0, Math.max(2, ((loopOut - loopIn) / duration) * w), h);
    }
    ctx.fillStyle = "rgba(0,0,0,0.45)";
    ctx.fillRect(0, 0, (position / duration) * w, h);
    // Cue and hot cue markers: small triangles along the top edge.
    const tri = (t: number, colour: string) => {
      const px = (t / duration) * w;
      ctx.fillStyle = colour;
      ctx.beginPath();
      ctx.moveTo(px - 5 * dpr, 0);
      ctx.lineTo(px + 5 * dpr, 0);
      ctx.lineTo(px, 7 * dpr);
      ctx.fill();
    };
    tri(cuePoint, "#ff8c28");
    hotcues.forEach((t, i) => t != null && tri(t, HOTCUE_COLOURS[i]));
    ctx.fillStyle = "#fff";
    ctx.fillRect((position / duration) * w - dpr, 0, 2 * dpr, h);
  }
}
