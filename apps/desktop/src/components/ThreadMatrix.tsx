import { memo, type CSSProperties } from "react";
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

/**
 * The 8x8 connection matrix: one cell per connection slot (up to 64). Cells of the connections that are
 * transferring flicker at a rate set by their throughput; the animation is pure CSS (opacity only, so
 * it stays on the compositor) and the cells are offset by nth-child rules so they don't blink in step.
 * Re-rendered only when the lit count, the state or the pulse bucket changes.
 */
export const ThreadMatrix = memo(function ThreadMatrix({ connections, speedBps, status, panel = false }: { connections: number; speedBps: number; status: DownloadStatus; panel?: boolean }) {
  const t = useT();
  const live = status === "downloading";
  const lit = Math.min(64, Math.max(0, connections));
  const state = status === "completed" ? "done" : status === "paused" ? "paused" : status === "failed" ? "failed" : live ? "live" : "idle";
  // rounded to 0.1 s so a small speed change doesn't restart the CSS animation
  const pulse = Math.round(pulseSeconds(speedBps, lit) * 10) / 10;
  // finished: every slot did its part
  const shown = status === "completed" ? 64 : live ? lit : 0;
  if (panel) {
    return (
      <div className="mx-panel" data-testid="thread-matrix">
        <div className="mb-2 flex items-center justify-between gap-4 text-[11.5px] font-medium text-fg/80">
          <span>{t("card.threadsN", { n: 64 })}</span>
          <span className="num ltr">
            {shown}/64
          </span>
        </div>
        <div className="matrix" data-state={state} role="img" aria-label={t("card.threads", { n: live ? lit : 0 })} style={{ "--pulse": `${pulse}s` } as CSSProperties}>
          {CELLS.map((i) => (
            <i key={i} className={i < lit ? "on" : undefined} />
          ))}
        </div>
      </div>
    );
  }
  return (
    <div className="flex flex-col items-center gap-1.5" data-testid="thread-matrix">
      <div className="matrix" data-state={state} role="img" aria-label={t("card.threads", { n: live ? lit : 0 })} style={{ "--pulse": `${pulse}s` } as CSSProperties}>
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
