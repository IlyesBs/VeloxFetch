import { memo, useLayoutEffect, useRef } from "react";
import { useT } from "../i18n";
import type { DownloadStatus } from "../types";

const CELLS = Array.from({ length: 64 }, (_, i) => i);

/**
 * How fast a lit cell pulses, from the throughput of one connection: about 2.4 s when a connection
 * trickles, down to under half a second when it is fast.
 */
export function pulseSeconds(speedBps: number, connections: number): number {
  const perKiB = speedBps / Math.max(1, connections) / 1024;
  return Math.max(0.45, 2.4 / (1 + Math.log2(1 + perKiB / 64)));
}

/** The pulse the stylesheet's timings are written for (`.matrix { --pulse: 1.6s }` in styles.css). */
export const BASE_PULSE_S = 1.6;

/** Minimal view of a running animation (the Web Animations API; `animationName` on CSS animations only). */
type Timed = { playbackRate: number; animationName?: string };

/** The cells' pulse (`@keyframes flicker`); their colour transition keeps its own pace. */
const PULSE = "flicker";

/**
 * Speed the lit cells' animations up or down to `pulse` seconds per beat. The speed changes through
 * `playbackRate`, which keeps each animation where it is: changing the duration instead (the `--pulse` variable)
 * restarted every lit cell at once, so the whole grid blinked each time a starting download's speed moved.
 */
export function retime(animations: Iterable<Timed>, pulse: number): number {
  const rate = BASE_PULSE_S / pulse;
  let changed = 0;
  for (const a of animations) {
    if (a.animationName === PULSE && Math.abs(a.playbackRate - rate) > 1e-6) {
      a.playbackRate = rate;
      changed++;
    }
  }
  return changed;
}

function cellAnimations(grid: HTMLElement | null): Timed[] {
  if (!grid || typeof grid.getAnimations !== "function") return [];
  return grid.getAnimations({ subtree: true });
}

/**
 * The 8x8 connection matrix: one cell per connection slot (up to 64). Cells of the connections that are
 * transferring flicker at a rate set by their throughput; the animation is pure CSS (opacity only, so
 * it stays on the compositor) and the cells are offset by nth-child rules so they don't blink in step.
 * Re-rendered only when the lit count, the state or the pulse bucket changes; the pace changes without a restart
 * (`retime`).
 */
export const ThreadMatrix = memo(function ThreadMatrix({ connections, speedBps, status, panel = false }: { connections: number; speedBps: number; status: DownloadStatus; panel?: boolean }) {
  const t = useT();
  const live = status === "downloading";
  const lit = Math.min(64, Math.max(0, connections));
  const state = status === "completed" ? "done" : status === "paused" ? "paused" : status === "failed" ? "failed" : live ? "live" : "idle";
  // rounded to 0.1 s so a small speed change doesn't re-render the grid
  const pulse = Math.round(pulseSeconds(speedBps, lit) * 10) / 10;
  // finished: every slot did its part
  const shown = status === "completed" ? 64 : live ? lit : 0;
  const grid = useRef<HTMLDivElement>(null);
  // after every change of what is lit: newly lit cells start at the stylesheet's pace, set them to this one
  useLayoutEffect(() => void retime(cellAnimations(grid.current), pulse), [pulse, lit, state]);
  if (panel) {
    return (
      <div className="mx-panel" data-testid="thread-matrix">
        <div className="mb-2 flex items-center justify-between gap-4 text-[11.5px] font-medium text-fg/80">
          <span>{t("card.threadsN", { n: 64 })}</span>
          <span className="num ltr">
            {shown}/64
          </span>
        </div>
        <div className="matrix" data-state={state} role="img" aria-label={t("card.threads", { n: live ? lit : 0 })} ref={grid}>
          {CELLS.map((i) => (
            <i key={i} className={i < lit ? "on" : undefined} />
          ))}
        </div>
      </div>
    );
  }
  return (
    <div className="flex flex-col items-center gap-1.5" data-testid="thread-matrix">
      <div className="matrix" data-state={state} role="img" aria-label={t("card.threads", { n: live ? lit : 0 })} ref={grid}>
        {CELLS.map((i) => (
          <i key={i} className={i < lit ? "on" : undefined} />
        ))}
      </div>
      <span className="num text-[10px] font-medium text-dim">
        {live ? lit : 0}
        <span className="opacity-60">/64</span>
      </span>
    </div>
  );
});
