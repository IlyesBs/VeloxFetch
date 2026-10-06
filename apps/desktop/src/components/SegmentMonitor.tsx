import { t as tr } from "../i18n";
import { memo, useEffect, useRef } from "react";
import { formatBytes } from "../format";
import { onThemeChange, readPalette, type Palette } from "../theme";
import type { Segment, SegmentStatus } from "../types";
import { ProgressBar } from "../ui/primitives";

const HEIGHT = 44;

function colorFor(status: SegmentStatus, p: Palette): string {
  switch (status) {
    case "completed":
      return p.ok;
    case "retrying":
      return p.warn;
    case "failed":
      return p.bad;
    case "paused":
    case "cancelled":
      return p.dim;
    default:
      return p.accent;
  }
}

const fracOf = (s: Segment) => Math.min(1, s.downloaded / Math.max(1, s.end - s.start + 1));

/**
 * The whole file as a strip of chunks, drawn on a canvas.
 *
 * Data arrives ~4 times a second, but the picture is animated at display rate: each chunk's
 * displayed fill eases toward its latest value in a requestAnimationFrame loop. The DOM is never
 * touched on update (only the canvas is repainted), and the loop stops by itself once everything
 * has settled, so an idle detail view costs nothing.
 */
export function SegmentMap({ segments }: { segments: Segment[] }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const segs = useRef<Segment[]>([]);
  const shown = useRef<number[]>([]); // displayed fill per chunk (eased)
  const raf = useRef(0);
  const last = useRef(0);
  const palette = useRef<Palette | null>(null);

  const draw = () => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const dpr = window.devicePixelRatio || 1;
    const W = canvas.clientWidth;
    if (canvas.width !== Math.round(W * dpr) || canvas.height !== Math.round(HEIGHT * dpr)) {
      canvas.width = Math.round(W * dpr);
      canvas.height = Math.round(HEIGHT * dpr);
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, W, HEIGHT);
    const p = (palette.current ??= readPalette());
    const list = segs.current;
    const n = list.length;
    if (n === 0) return;
    const gap = n > 48 ? 1 : 2;
    const total = list.reduce((a, s) => a + (s.end - s.start + 1), 0) || 1;
    const usable = W - gap * (n - 1);
    let x = 0;
    for (let i = 0; i < n; i++) {
      const s = list[i]!;
      const w = Math.max(1, ((s.end - s.start + 1) / total) * usable);
      const r = Math.min(4, w / 2);
      ctx.globalAlpha = 0.35;
      ctx.fillStyle = p.track;
      ctx.beginPath();
      ctx.roundRect(x, 0, w, HEIGHT, r);
      ctx.fill();
      const f = shown.current[i] ?? 0;
      if (f > 0.001) {
        ctx.globalAlpha = 1;
        ctx.fillStyle = colorFor(s.status, p);
        ctx.beginPath();
        ctx.roundRect(x, 0, Math.max(1, w * f), HEIGHT, r);
        ctx.fill();
        if (s.status === "downloading" && f < 1) {
          // bright leading edge so the active frontier is easy to see
          ctx.globalAlpha = 0.9;
          ctx.fillStyle = "#fff";
          ctx.fillRect(x + w * f - 1.5, 3, 1.5, HEIGHT - 6);
        }
      }
      x += w + gap;
    }
    ctx.globalAlpha = 1;
  };

  const frame = (t: number) => {
    const dt = Math.min(64, t - (last.current || t));
    last.current = t;
    const k = 1 - Math.exp(-dt / 90); // ~90 ms time constant, independent of frame rate
    let moving = false;
    const list = segs.current;
    for (let i = 0; i < list.length; i++) {
      const target = fracOf(list[i]!);
      const cur = shown.current[i] ?? 0;
      let next = cur + (target - cur) * k;
      if (Math.abs(target - next) < 0.002) next = target;
      else moving = true;
      shown.current[i] = next;
    }
    draw();
    if (moving) raf.current = requestAnimationFrame(frame);
    else {
      raf.current = 0;
      last.current = 0;
    }
  };

  const kick = () => {
    if (!raf.current) raf.current = requestAnimationFrame(frame);
  };

  // New data: retarget and (re)start the easing loop.
  useEffect(() => {
    segs.current = segments;
    if (shown.current.length !== segments.length) shown.current.length = segments.length;
    kick();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [segments]);

  // Repaint on resize / theme change.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ro = new ResizeObserver(kick);
    ro.observe(canvas);
    const off = onThemeChange(() => {
      palette.current = null;
      kick();
    });
    return () => {
      ro.disconnect();
      off();
      cancelAnimationFrame(raf.current);
      raf.current = 0;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const done = segments.filter((s) => s.status === "completed").length;
  return (
    <canvas
      ref={canvasRef}
      role="img"
      aria-label={tr("seg.map", { done, total: segments.length })}
      data-testid="segment-map"
      className="block h-11 w-full"
    />
  );
}

const Row = memo(
  function Row({ s }: { s: Segment }) {
    const f = fracOf(s);
    const tone = s.status === "completed" ? "ok" : s.status === "retrying" ? "warn" : s.status === "failed" ? "bad" : s.status === "paused" ? "dim" : "accent";
    return (
      <li className="grid grid-cols-[4.5rem_1fr_3rem_6rem] items-center gap-3 py-1 text-xs" data-testid="segment-row">
        <span className="text-dim">Chunk {s.index + 1}</span>
        <ProgressBar value={f} tone={tone} label={tr("seg.chunk", { n: s.index + 1 })} />
        <span className="num text-end font-medium">{Math.floor(f * 100)}%</span>
        <span className="num text-end text-dim">{formatBytes(s.end - s.start + 1)}</span>
      </li>
    );
  },
  // Re-render a row only when its visible values change (percent step, status, retries).
  (a, b) =>
    a.s.status === b.s.status &&
    a.s.retries === b.s.retries &&
    Math.floor(fracOf(a.s) * 1000) === Math.floor(fracOf(b.s) * 1000),
);

export function SegmentRows({ segments }: { segments: Segment[] }) {
  return (
    <ul className="max-h-72 divide-y divide-line overflow-y-auto pe-1" data-testid="segment-rows">
      {segments.map((s) => (
        <Row key={s.index} s={s} />
      ))}
    </ul>
  );
}
