// Scrolling colour waveform. The summary (4 bytes per bin: peak, low, mid, high) is
// uploaded once as a texture; a fragment shader draws the visible window each frame,
// so per-frame cost does not depend on track length. Beats, cues, loops and the
// playhead go on a 2D canvas on top (only a few dozen lines per frame).

import type { GridDto } from "./types";

const TEX_W = 4096;

const VS = `#version 300 es
in vec2 p;
void main() { gl_Position = vec4(p, 0.0, 1.0); }`;

const FS = `#version 300 es
precision highp float;
uniform sampler2D uWave;
uniform float uCentre;     // playhead, in bins
uniform float uBinsPerPx;
uniform vec2 uSize;        // canvas size in device pixels
uniform float uBins;
out vec4 color;
void main() {
  vec2 frag = gl_FragCoord.xy;
  float bin = floor(uCentre + (frag.x - uSize.x * 0.5) * uBinsPerPx);
  vec3 bg = vec3(0.03, 0.035, 0.045);
  if (bin < 0.0 || bin >= uBins) { color = vec4(bg, 1.0); return; }
  vec4 w = texelFetch(uWave, ivec2(int(mod(bin, ${TEX_W}.0)), int(bin / ${TEX_W}.0)), 0);
  float dy = abs(frag.y - uSize.y * 0.5) / (uSize.y * 0.5);
  // Three bands, low in blue, mid in amber, high in white, stacked from the centre out.
  vec3 low = vec3(0.12, 0.45, 1.0), mid = vec3(1.0, 0.62, 0.18), high = vec3(0.95, 0.95, 1.0);
  float tot = w.y + w.z + w.w + 1e-4;
  vec3 c = (low * w.y + mid * w.z + high * w.w) / tot;
  float played = frag.x < uSize.x * 0.5 ? 0.55 : 1.0;
  if (dy < w.x) {
    float shade = 0.65 + 0.35 * (1.0 - dy / max(w.x, 1e-3));
    color = vec4(c * shade * played, 1.0);
  } else {
    color = vec4(bg, 1.0);
  }
}`;

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
  private tex: WebGLTexture;
  private u: Record<string, WebGLUniformLocation | null> = {};
  private bins = 0;
  private binsPerSecond = 150;

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
    for (const n of ["uWave", "uCentre", "uBinsPerPx", "uSize", "uBins"]) this.u[n] = gl.getUniformLocation(prog, n);
    const buf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
    const loc = gl.getAttribLocation(prog, "p");
    gl.enableVertexAttribArray(loc);
    gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
    this.tex = gl.createTexture()!;
  }

  setWaveform(data: Uint8Array, binsPerSecond: number) {
    const gl = this.gl;
    this.bins = data.length / 4;
    this.binsPerSecond = binsPerSecond;
    const rows = Math.max(1, Math.ceil(this.bins / TEX_W));
    const padded = new Uint8Array(TEX_W * rows * 4);
    padded.set(data);
    gl.bindTexture(gl.TEXTURE_2D, this.tex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, TEX_W, rows, 0, gl.RGBA, gl.UNSIGNED_BYTE, padded);
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
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.tex);
    gl.uniform1i(this.u.uWave, 0);
    gl.uniform1f(this.u.uCentre, f.position * this.binsPerSecond);
    gl.uniform1f(this.u.uBinsPerPx, (2 * f.halfWindow * this.binsPerSecond) / w);
    gl.uniform2f(this.u.uSize, w, h);
    gl.uniform1f(this.u.uBins, this.bins);
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
      let hi = 0;
      for (let i = a; i < b; i++) {
        peak = Math.max(peak, data[i * 4]);
        lo += data[i * 4 + 1];
        hi += data[i * 4 + 3];
      }
      const n = b - a;
      const amp = (peak / 255) * (h / 2);
      c.fillStyle = `rgb(${80 + (hi / n) * 0.6},${110 + (lo / n) * 0.2},${200})`;
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
