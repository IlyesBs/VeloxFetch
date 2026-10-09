// The window's backdrop (the "aurora": five soft radial lights behind everything, `.aurora` in styles.css), drawn once
// into a picture instead of being computed by the browser on every repaint.
//
// Why: every panel over it is see-through, so each repaint (a running download's matrix, a scroll step) redrew the
// five window-sized gradients under the changed area. On Linux at a fractional screen scale (GTK draws at 2x) that was
// half of every frame: 65 ms per frame with downloads running, 17 fps while scrolling; 30 ms without it. A picture is
// copied, not computed. Same lights, same colours (read from the theme), redrawn when the theme, the accent (Smart
// throttle's amber) or the window size changes. If anything fails, the CSS gradients underneath stay.

/** One light: an ellipse `w` x `h` (rem) centred at `x`, `y` (share of the window), fading out at `stop`. */
export interface Light {
  w: number;
  h: number;
  x: number;
  y: number;
  color: string;
  stop: number;
}

/** The same lights as `.aurora` in styles.css, in the same order (the first is on top). */
export const LIGHTS: Light[] = [
  { w: 46, h: 22, x: 0.3, y: -0.06, color: "--sky-3", stop: 0.7 },
  { w: 38, h: 26, x: 0.12, y: 0.08, color: "--glow", stop: 0.7 },
  { w: 34, h: 28, x: 0.88, y: 0.22, color: "--glow-2", stop: 0.72 },
  { w: 44, h: 30, x: 0.7, y: 1.05, color: "--sky-2", stop: 0.7 },
  { w: 30, h: 24, x: 0, y: 0.9, color: "--sky-1", stop: 0.72 },
];

/** "rgba(1, 2, 3, 0.4)" / "rgb(1 2 3 / 0.4)" -> the same colour fully transparent (CSS fades to `transparent`
 *  in premultiplied colour, which is this colour at alpha 0, not black). */
export function transparentOf(color: string): string {
  const m = /rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)/.exec(color);
  return m ? `rgba(${m[1]}, ${m[2]}, ${m[3]}, 0)` : "rgba(0, 0, 0, 0)";
}

/** Draw the lights on `ctx` for a window `width` x `height` (CSS px), `rem` px per rem; `colorOf` resolves a variable. */
export function drawAurora(ctx: CanvasRenderingContext2D, width: number, height: number, rem: number, colorOf: (v: string) => string): void {
  ctx.clearRect(0, 0, width, height);
  for (const l of [...LIGHTS].reverse()) {
    const color = colorOf(l.color);
    const rx = l.w * rem;
    const ry = l.h * rem;
    ctx.save();
    ctx.translate(l.x * width, l.y * height);
    ctx.scale(1, ry / rx);
    const g = ctx.createRadialGradient(0, 0, 0, 0, 0, rx);
    g.addColorStop(0, color);
    g.addColorStop(l.stop, transparentOf(color));
    g.addColorStop(1, transparentOf(color));
    ctx.fillStyle = g;
    ctx.fillRect(-rx, -rx, rx * 2, rx * 2);
    ctx.restore();
  }
}

/** Resolve the lights' colour variables to "rgb(...)"/"rgba(...)" through a hidden probe element. */
function resolveColors(): Record<string, string> {
  const probe = document.createElement("span");
  probe.style.display = "none";
  document.body.appendChild(probe);
  const out: Record<string, string> = {};
  for (const l of LIGHTS) {
    probe.style.color = `var(${l.color})`;
    out[l.color] = getComputedStyle(probe).color;
  }
  probe.remove();
  return out;
}

/** Keep `el`'s background a picture of the aurora. Returns a cleanup. */
export function paintAurora(el: HTMLElement, onThemeChange: (cb: () => void) => () => void): () => void {
  let url: string | null = null;
  let pending = 0;
  const paint = () => {
    pending = 0;
    try {
      const width = Math.max(1, window.innerWidth);
      const height = Math.max(1, window.innerHeight);
      // At the screen's own pixels: a picture that has to be stretched on every repaint costs more than it saves.
      const scale = window.devicePixelRatio || 1;
      const canvas = document.createElement("canvas");
      canvas.width = Math.ceil(width * scale);
      canvas.height = Math.ceil(height * scale);
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      ctx.scale(scale, scale);
      const colors = resolveColors();
      drawAurora(ctx, width, height, parseFloat(getComputedStyle(document.documentElement).fontSize) || 16, (v) => colors[v] ?? "rgba(0, 0, 0, 0)");
      canvas.toBlob((blob) => {
        if (!blob) return;
        const next = URL.createObjectURL(blob);
        el.style.backgroundImage = `url("${next}")`;
        el.style.backgroundSize = "100% 100%";
        el.style.backgroundRepeat = "no-repeat";
        if (url) URL.revokeObjectURL(url);
        url = next;
      });
    } catch {
      /* the CSS gradients stay */
    }
  };
  const later = () => {
    if (pending) clearTimeout(pending);
    pending = window.setTimeout(paint, 120);
  };
  paint();
  // The accent changes colour with a short transition: paint once more when it has settled.
  const stopTheme = onThemeChange(() => {
    later();
    window.setTimeout(paint, 450);
  });
  window.addEventListener("resize", later);
  return () => {
    stopTheme();
    window.removeEventListener("resize", later);
    if (pending) clearTimeout(pending);
    if (url) URL.revokeObjectURL(url);
    el.style.backgroundImage = "";
  };
}
