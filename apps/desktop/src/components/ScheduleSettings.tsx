import { useEffect, useState } from "react";
import { actions } from "../actions";
import { useStore } from "../hooks";
import { t as tr, useT, type Key } from "../i18n";
import type { Schedule } from "../types";
import { Panel } from "../ui/primitives";
import { formatWhen } from "./when";

const DAYS: Key[] = ["day.0", "day.1", "day.2", "day.3", "day.4", "day.5", "day.6"];
const TIME = /^([01]\d|2[0-3]):[0-5]\d$/;

function same(a: Schedule, b: Schedule): boolean {
  return a.enabled === b.enabled && a.start === b.start && a.stop === b.stop && a.days.every((d, i) => d === b.days[i]);
}

/** Why the draft can't be saved, or null. The backend validates again. */
export function scheduleProblem(s: Schedule): string | null {
  if (!TIME.test(s.start) || !TIME.test(s.stop)) return tr("sched.badTime");
  if (s.start === s.stop) return tr("sched.same");
  if (!s.days.some(Boolean)) return tr("sched.noDay");
  return null;
}

/** "Start at 22:00, stop at 07:00": downloads only run inside the window. */
export function ScheduleSettings() {
  const t = useT();
  const q = useStore((s) => s.queue);
  const saved = q?.schedule;
  const [draft, setDraft] = useState<Schedule | null>(saved ?? null);

  // Follow the saved schedule (first load, or a change made elsewhere).
  const key = saved ? JSON.stringify(saved) : "";
  useEffect(() => {
    if (saved) setDraft(saved);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  if (!q || !draft || !saved) {
    return (
      <Panel title={t("sched.title")} hint={t("sched.hint")}>
        <p className="text-xs text-dim">{t("common.loading")}</p>
      </Panel>
    );
  }

  const problem = scheduleProblem(draft);
  const dirty = !same(draft, saved);
  const edit = (patch: Partial<Schedule>) => setDraft({ ...draft, ...patch });

  let state = t("sched.off");
  if (saved.enabled) {
    state = q.scheduleOpen
      ? `${t("sched.inside")}${q.nextChange ? ` · ${t("sched.stops", { when: formatWhen(q.nextChange) })}` : ""}`
      : `${t("sched.outside")}${q.nextChange ? ` · ${t("sched.starts", { when: formatWhen(q.nextChange) })}` : ""}`;
  }

  return (
    <Panel title={t("sched.title")} hint={t("sched.hint")}>
      <label className="flex w-fit items-center gap-2 text-[13px] font-medium">
        <input
          type="checkbox"
          data-testid="schedule-enabled"
          checked={draft.enabled}
          onChange={(e) => edit({ enabled: e.target.checked })}
        />
        {t("sched.use")}
      </label>

      <div className="mt-4 flex flex-wrap items-end gap-4">
        <label className="block text-xs text-dim">
          {t("sched.start")}
          <input
            type="time"
            className="input mt-1 w-32"
            data-testid="schedule-start"
            value={draft.start}
            onChange={(e) => edit({ start: e.target.value })}
          />
        </label>
        <label className="block text-xs text-dim">
          {t("sched.stop")}
          <input
            type="time"
            className="input mt-1 w-32"
            data-testid="schedule-stop"
            value={draft.stop}
            onChange={(e) => edit({ stop: e.target.value })}
          />
        </label>
        <div role="group" aria-label={t("sched.days")} className="flex flex-wrap gap-1">
          {DAYS.map((d, i) => (
            <button
              key={d}
              type="button"
              aria-pressed={draft.days[i]}
              data-testid={`schedule-day-${i}`}
              onClick={() => edit({ days: draft.days.map((on, j) => (j === i ? !on : on)) })}
              className={`h-8 rounded-lg px-2.5 text-xs font-medium transition-colors duration-150 ${
                draft.days[i] ? "bg-accent-soft text-accent" : "border border-line text-dim hover:text-fg"
              }`}
            >
              {t(d)}
            </button>
          ))}
        </div>
      </div>

      <p className="mt-3 text-xs text-dim">
        {t("sched.how")}
      </p>
      <p className="mt-2 text-[13px]" data-testid="schedule-status" role="status">
        {state}
      </p>

      <div className="mt-3 flex items-center gap-3">
        <button
          className="btn btn-primary"
          data-testid="schedule-save"
          disabled={!dirty || problem !== null}
          onClick={() => void actions.setSchedule(draft)}
        >
          {t("sched.save")}
        </button>
        {problem && dirty && (
          <span className="text-xs text-bad" role="alert">
            {problem}
          </span>
        )}
      </div>
    </Panel>
  );
}
