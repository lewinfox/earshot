// Frame-time measurement for the HUD and the benchmark.

export class FrameStats {
  private times: number[] = [];

  add(dtMs: number) {
    this.times.push(dtMs);
  }

  reset() {
    this.times = [];
  }

  /** Keep only the last `ms` worth of frames. */
  trim(ms: number) {
    let total = 0;
    let i = this.times.length;
    while (i > 0 && total < ms) total += this.times[--i];
    if (i > 0) this.times = this.times.slice(i);
  }

  summary() {
    const t = this.times;
    if (!t.length) return { frames: 0, avgFps: 0, low1Fps: 0, worstMs: 0, over20ms: 0, over33ms: 0 };
    const sorted = [...t].sort((a, b) => a - b);
    const total = t.reduce((a, b) => a + b, 0);
    const p99 = sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * 0.99))];
    return {
      frames: t.length,
      avgFps: (t.length / total) * 1000,
      low1Fps: 1000 / p99,
      worstMs: sorted[sorted.length - 1],
      over20ms: t.filter((x) => x > 20).length,
      over33ms: t.filter((x) => x > 33.4).length,
    };
  }
}

/** WebGL renderer string, if the browser exposes it. */
export function gpuInfo(): string {
  try {
    const gl = document.createElement("canvas").getContext("webgl2");
    if (!gl) return "no WebGL2";
    const ext = gl.getExtension("WEBGL_debug_renderer_info");
    const r = ext ? gl.getParameter(ext.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER);
    const v = ext ? gl.getParameter(ext.UNMASKED_VENDOR_WEBGL) : gl.getParameter(gl.VENDOR);
    gl.getExtension("WEBGL_lose_context")?.loseContext();
    return `${v} / ${r}`;
  } catch (e) {
    return String(e);
  }
}
