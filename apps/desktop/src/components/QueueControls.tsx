import { useState } from "react";
import { actions } from "../actions";
import { useStore } from "../hooks";
import { useT } from "../i18n";
import { Icon } from "../ui/icons";
import { formatWhen } from "./when";

const ACTIVE_CHOICES = [1, 2, 3, 4, 5, 6, 8, 10, 12, 16, 24, 32];
const CONNECTION_CHOICES = [1, 2, 4, 8, 16, 24, 32, 48, 64];

/** Options always include the current value, even if it is not one of the presets. */
function withCurrent(choices: number[], current: number): number[] {
  return choices.includes(current) ? choices : [...choices, current].sort((a, b) => a - b);
}

/** Queue limits and pause / resume, shown at the top of the Queue view. */
export function QueueControls() {
  const t = useT();
  const q = useStore((s) => s.queue);
  // Live counts come from the download list itself; the queue state only changes on queue events.
  const running = useStore((s) => s.groups.active.length);
  const waiting = useStore((s) => s.groups.queued.length);
  const [pauseRunning, setPauseRunning] = useState(true);
  if (!q) return null;

  const waitingForWindow = q.schedule.enabled && !q.scheduleOpen;
  let status: string;
  if (q.paused) {
    status = q.held > 0 ? t("qc.pausedHeld", { n: q.held }) : t("qc.paused");
  } else if (waitingForWindow) {
    status = q.nextChange ? t("qc.outsideOpens", { when: formatWhen(q.nextChange) }) : t("qc.outside");
  } else {
    status = t("qc.running", { r: running, w: waiting });
  }
  const idle = q.paused || waitingForWindow;

  return (
    <section className="card p-4" aria-label={t("qc.label")} data-testid="queue-controls">
      <div className="flex flex-wrap items-center gap-3">
        <span
          className={`grid h-8 w-8 place-items-center rounded-lg ${idle ? "bg-warn-soft text-warn" : "bg-accent-soft text-accent"}`}
          aria-hidden="true"
        >
          <Icon name={idle ? "pause" : "play"} size={16} />
        </span>
        <p className="min-w-0 flex-1 text-[13px] font-medium" data-testid="queue-status" role="status">
          {status}
        </p>
        {q.paused ? (
          <button className="btn btn-primary" data-testid="queue-resume" onClick={() => void actions.resumeQueue()}>
            <Icon name="play" size={14} /> {t("qc.resume")}
          </button>
        ) : (
          <>
            <label className="flex items-center gap-1.5 text-xs text-dim">
              <input
                type="checkbox"
                data-testid="queue-pause-running"
                checked={pauseRunning}
                onChange={(e) => setPauseRunning(e.target.checked)}
              />
              {t("qc.alsoRunning")}
            </label>
            <button className="btn" data-testid="queue-pause" onClick={() => void actions.pauseQueue(pauseRunning)}>
              <Icon name="pause" size={14} /> {t("qc.pause")}
            </button>
          </>
        )}
      </div>

      <div className="mt-4 grid gap-3 sm:grid-cols-2">
        <label className="block text-xs text-dim">
          {t("qc.maxActive")}
          <select
            className="input mt-1"
            data-testid="queue-max-active"
            value={q.maxActive}
            onChange={(e) => void actions.setMaxActive(Number(e.target.value))}
          >
            {withCurrent(ACTIVE_CHOICES, q.maxActive).map((n) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
          </select>
        </label>
        <label className="block text-xs text-dim">
          {t("qc.maxConn")}
          <select
            className="input mt-1"
            data-testid="queue-max-connections"
            value={q.maxConnections}
            onChange={(e) => void actions.setMaxConnections(Number(e.target.value))}
          >
            {withCurrent(CONNECTION_CHOICES, q.maxConnections).map((n) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
          </select>
        </label>
      </div>
      <p className="mt-2 text-xs text-dim">
        {t("qc.how")}
      </p>
    </section>
  );
}
