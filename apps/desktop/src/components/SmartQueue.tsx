import { memo, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { actions } from "../actions";
import { formatEta, fraction, hostOf, percent } from "../format";
import { useDownload, useStore } from "../hooks";
import { t as tr, useT, type Key } from "../i18n";
import {
  canMove,
  localStamp,
  nextDownload,
  queueName,
  QUEUE_PRIORITIES,
  quickTimes,
  ruleChips,
  timelineBars,
  timelinePoint,
  visualState,
  waitReason,
  type VisualState,
  type WaitReason,
} from "../smartQueue";
import type { Download, Priority, QueueInfo, Queues } from "../types";
import { Icon, type IconName } from "../ui/icons";
import { Menu, ProgressBar, type MenuItem } from "../ui/primitives";
import { PrimaryAction } from "./DownloadCard";
import { formatWhen } from "./when";

const STATE_STYLE: Record<VisualState, { chip: string; icon: IconName }> = {
  downloading: { chip: "bg-accent-soft text-accent", icon: "download" },
  retrying: { chip: "bg-warn/15 text-warn", icon: "retry" },
  waiting: { chip: "bg-line text-dim", icon: "clock" },
  scheduled: { chip: "bg-accent-soft text-accent", icon: "calendar" },
  paused: { chip: "bg-warn/15 text-warn", icon: "pause" },
  completed: { chip: "bg-ok/15 text-ok", icon: "check" },
  failed: { chip: "bg-bad/15 text-bad", icon: "alert" },
};

/** The clear state of a download in the queue (Downloading, Waiting, Scheduled, Paused, Completed, Failed, Retrying). */
export function QueueStateChip({ state }: { state: VisualState }) {
  const t = useT();
  const s = STATE_STYLE[state];
  return (
    <span className={`chip shrink-0 ${s.chip}`} data-state={state}>
      {state === "downloading" ? <span className="live-dot h-1.5 w-1.5 rounded-full bg-current" /> : <Icon name={s.icon} size={11} />}
      {t(`sq.state.${state}` as Key)}
    </span>
  );
}

/** Why it waits, in a few words. */
export function reasonText(why: WaitReason, d: Download | null, q: QueueInfo | undefined): string {
  if (why === "at" && d?.startAt) return tr("sq.why.at", { when: formatWhen(d.startAt) });
  if (why === "time") return q?.opensAt ? tr("sq.why.at", { when: formatWhen(q.opensAt) }) : tr("sq.why.time");
  return tr(`sq.why.${why}` as Key);
}

// ------------------------------------------------------------------ tabs

/** One pill per queue (its name, how many downloads still to go, a dot when it is downloading) and "New queue". */
export function QueueTabs({ queues, selected, counts, active, onSelect, onNew }: {
  queues: QueueInfo[];
  selected: number;
  counts: Record<number, number>;
  active: Record<number, boolean>;
  onSelect: (id: number) => void;
  onNew: () => void;
}) {
  const t = useT();
  return (
    <div className="flex items-center gap-2">
      <div role="group" aria-label={t("sq.tabs")} className="tabbar grow" data-testid="sq-tabs">
        {queues.map((q) => (
          <button key={q.id} type="button" className="tab" aria-pressed={q.id === selected} data-testid={`sq-tab-${q.id}`} onClick={() => onSelect(q.id)}>
            {active[q.id] && <span className="live-dot me-1 h-1.5 w-1.5 rounded-full bg-accent" aria-hidden="true" />}
            {q.gate !== "open" && !active[q.id] && <Icon name={q.gate === "paused" ? "pause" : q.gate === "time" ? "calendar" : "clock"} size={13} className="me-0.5 text-dim" />}
            {queueName(q)}
            {(counts[q.id] ?? 0) > 0 && <span className="num ms-1 rounded-full bg-accent-soft px-1.5 text-[11px] font-semibold text-accent">{counts[q.id]}</span>}
          </button>
        ))}
      </div>
      <button type="button" className="btn shrink-0" data-testid="sq-new" onClick={onNew}>
        <Icon name="plus" size={14} /> {t("sq.new")}
      </button>
    </div>
  );
}

// ------------------------------------------------------------------ header

function gateText(q: QueueInfo): string {
  if (q.forced && q.gate === "open") return tr("sq.forced");
  if (q.gate === "time") return q.opensAt ? tr("sq.gate.timeAt", { when: formatWhen(q.opensAt) }) : tr("sq.gate.time");
  return tr(`sq.gate.${q.gate}` as Key);
}

function nextText(n: NonNullable<ReturnType<typeof nextDownload>>, name: string): string {
  switch (n.when.kind) {
    case "now":
      return tr("sq.nextNow", { name });
    case "slot":
      return n.when.etaSecs !== null && n.when.etaSecs > 0 ? tr("sq.nextSlotEta", { name, eta: formatEta(n.when.etaSecs) }) : tr("sq.nextSlot", { name });
    case "at":
      return tr("sq.nextAt", { name, when: formatWhen(n.when.at) });
    case "reason":
      return tr("sq.nextReason", { name, reason: reasonText(n.when.reason, null, undefined).toLowerCase() });
  }
}

/** The selected queue: its state, its rules, its actions, the next download and (when it has times) the timeline. */
export function QueueHeader({ q, queues, order, byId, all, index, onEdit }: {
  q: QueueInfo;
  queues: Queues;
  order: number[];
  byId: Record<number, Download>;
  all: Download[];
  index: number;
  onEdit: () => void;
}) {
  const t = useT();
  const chips = ruleChips(q.rules);
  const next = nextDownload(order, byId, q, queues, all);
  const nextName = next ? byId[next.id]?.filename ?? "" : "";
  const completed = order.filter((id) => byId[id]?.status === "completed").length;
  const running = order.some((id) => ["downloading", "verifying"].includes(byId[id]?.status ?? ""));
  const scheduled = order
    .map((id) => byId[id])
    .filter((d): d is Download => !!d && !!d.startAt && timelinePoint(d.startAt, queues.now) !== null);
  const showTimeline = q.spans.length > 0 || scheduled.length > 0;
  const tone = q.gate === "open" ? (running ? "bg-accent-soft text-accent" : "bg-ok/15 text-ok") : q.gate === "time" ? "bg-accent-soft text-accent" : "bg-warn/15 text-warn";
  const more: MenuItem[] = [
    { label: t("sq.edit"), icon: "calendar", onSelect: onEdit },
    ...QUEUE_PRIORITIES.map((p) => ({ label: t("sq.prioAll", { p: t(`prio.${p}` as Key) }), icon: "sliders" as const, onSelect: () => void actions.queueAction(q.id, { kind: "priority", priority: p }) })),
    { label: t("sq.moveEarlier"), icon: "back", disabled: index === 0, onSelect: () => void actions.queueAction(q.id, { kind: "moveUp" }) },
    { label: t("sq.moveLater"), icon: "chevron", disabled: index === queues.queues.length - 1, onSelect: () => void actions.queueAction(q.id, { kind: "moveDown" }) },
  ];
  return (
    <section className="card rise p-4" data-testid="sq-header" aria-label={queueName(q)}>
      <div className="flex flex-wrap items-center gap-3">
        <span className={`grid h-9 w-9 shrink-0 place-items-center rounded-xl ${tone}`} aria-hidden="true">
          <Icon name={q.gate === "open" ? (running ? "download" : "check") : q.gate === "time" ? "calendar" : q.gate === "paused" ? "pause" : "clock"} size={17} />
        </span>
        <div className="min-w-0 flex-1">
          <h2 className="truncate text-[14px] font-semibold">{queueName(q)}</h2>
          <p className="truncate text-xs text-dim" role="status" data-testid="sq-gate">
            {gateText(q)}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-1.5">
          {q.paused || q.gate !== "open" ? (
            <button className="btn btn-primary" data-testid="sq-start" title={t("sq.startHint")} onClick={() => void actions.queueAction(q.id, { kind: "start" })}>
              <Icon name="play" size={14} /> {t("sq.start")}
            </button>
          ) : (
            <button className="btn" data-testid="sq-pause" onClick={() => void actions.queueAction(q.id, { kind: "pause" })}>
              <Icon name="pause" size={14} /> {t("sq.pause")}
            </button>
          )}
          <button className="btn" data-testid="sq-edit" onClick={onEdit}>
            <Icon name="calendar" size={14} /> {t("sq.edit")}
          </button>
          {completed > 0 && (
            <button className="btn" data-testid="sq-clear" title={t("sq.clearHint")} onClick={() => void actions.queueAction(q.id, { kind: "clearCompleted" }, tr("act.sqCleared", { n: completed }))}>
              <Icon name="broom" size={14} /> {t("sq.clear")}
            </button>
          )}
          <Menu label={t("sq.more")} items={more} />
        </div>
      </div>
      {chips.length > 0 && (
        <div className="mt-3 flex flex-wrap gap-1.5" data-testid="sq-rules">
          {chips.map((c) => (
            <span key={c} className="chip bg-line text-dim">
              {c}
            </span>
          ))}
        </div>
      )}
      {next && (
        <p className="mt-3 flex min-w-0 items-center gap-2 text-[13px]" data-testid="sq-next">
          <span className="label shrink-0">{t("sq.next")}</span>
          <span className="truncate">{nextText(next, nextName)}</span>
        </p>
      )}
      {showTimeline && <QueueTimeline q={q} now={queues.now} scheduled={scheduled} />}
    </section>
  );
}

// ------------------------------------------------------------------ timeline

/** The next 24 hours: when the queue may download (bars), when downloads are set to start (dots), now at the left. */
export function QueueTimeline({ q, now, scheduled }: { q: QueueInfo; now: string; scheduled: Download[] }) {
  const t = useT();
  const bars = timelineBars(q.spans, now);
  const nowHour = Number(now.slice(11, 13));
  const nowMin = Number(now.slice(14, 16));
  // Hour labels every 6 hours, on the hour.
  const ticks: { x: number; label: string }[] = [];
  for (let h = 1; h <= 24; h++) {
    const hour = (nowHour + h) % 24;
    if (hour % 6 !== 0) continue;
    ticks.push({ x: ((h * 60 - nowMin) / (24 * 60)) * 100, label: `${String(hour).padStart(2, "0")}:00` });
  }
  return (
    <figure className="mt-4" data-testid="sq-timeline">
      <figcaption className="mb-1.5 flex items-center justify-between text-[11px] text-dim">
        <span className="label">{t("sq.timeline")}</span>
        <span className="flex items-center gap-3">
          <span className="flex items-center gap-1">
            <span className="inline-block h-2 w-3 rounded-sm bg-accent/40" aria-hidden="true" />
            {t("sq.timelineOpen")}
          </span>
          {scheduled.length > 0 && (
            <span className="flex items-center gap-1">
              <span className="inline-block h-2 w-2 rounded-full bg-accent" aria-hidden="true" />
              {t("sq.timelineStart")}
            </span>
          )}
        </span>
      </figcaption>
      <div className="relative h-7 overflow-hidden rounded-lg border border-line bg-raised" dir="ltr">
        {ticks.map((k) => (
          <span key={k.label} className="absolute inset-y-0 border-s border-line" style={{ left: `${k.x}%` }} aria-hidden="true" />
        ))}
        {bars.map((b, i) => (
          <span
            key={i}
            className="bar-fill absolute inset-y-1 rounded-md bg-accent/35"
            style={{ left: `${b.left}%`, width: `${b.width}%` }}
            data-testid="sq-timeline-span"
            title={`${formatWhen(q.spans[i]!.from)} – ${formatWhen(q.spans[i]!.to)}`}
          />
        ))}
        {scheduled.map((d) => {
          const x = timelinePoint(d.startAt!, now)!;
          return (
            <span
              key={d.id}
              className="absolute top-1/2 h-2.5 w-2.5 -translate-x-1/2 -translate-y-1/2 rounded-full bg-accent ring-2 ring-solid"
              style={{ left: `${x}%` }}
              title={`${d.filename} · ${formatWhen(d.startAt!)}`}
              data-testid="sq-timeline-start"
            />
          );
        })}
        <span className="absolute inset-y-0 start-0 w-0.5 bg-fg/60" aria-hidden="true" />
      </div>
      <div className="relative mt-1 h-3 text-[10px] text-dim" dir="ltr" aria-hidden="true">
        <span className="absolute start-0">{t("sq.timelineNow")}</span>
        {ticks.map((k) => (
          <span key={k.label} className="num absolute -translate-x-1/2" style={{ left: `${k.x}%` }}>
            {k.label}
          </span>
        ))}
      </div>
    </figure>
  );
}

// ------------------------------------------------------------------ rows

/** One download in the queue: place, name, state and why, progress, priority, up / down, the main button and a menu. */
export const SmartQueueRow = memo(function SmartQueueRow({ id, position, order, q, queues, onOpen, onSchedule }: {
  id: number;
  position: number;
  order: number[];
  q: QueueInfo | undefined;
  queues: Queues | null;
  onOpen: (id: number) => void;
  onSchedule: (d: Download) => void;
}) {
  const t = useT();
  const d = useDownload(id);
  const byId = useStore((s) => s.byId);
  if (!d) return null;
  const state = visualState(d, q);
  const why = waitReason(d, q, queues);
  const done = state === "completed" || state === "failed";
  const frac = fraction(d.downloaded, d.totalSize);
  const showBar = !done && (d.downloaded > 0 || state === "downloading");
  const prios: Priority[] = QUEUE_PRIORITIES.includes(d.priority) ? QUEUE_PRIORITIES : [...QUEUE_PRIORITIES, d.priority];
  const others = (queues?.queues ?? []).filter((x) => x.id !== d.queueId);
  const menu: MenuItem[] = [
    ...(!done && state !== "downloading" && state !== "retrying" ? [{ label: t("sq.startNow"), icon: "play" as const, onSelect: () => void actions.place({ id: d.id, start: { kind: "now" } }) }] : []),
    ...(!done ? [{ label: t("sq.startAt"), icon: "calendar" as const, onSelect: () => onSchedule(d) }] : []),
    ...(!done && (d.startAt || state === "downloading" || state === "waiting") ? [{ label: t("sq.startLater"), icon: "pause" as const, onSelect: () => void actions.place({ id: d.id, start: { kind: "later" } }) }] : []),
    ...(!done ? others.map((x) => ({ label: t("sq.moveTo", { queue: queueName(x) }), icon: "queue" as const, onSelect: () => void actions.place({ id: d.id, queueId: x.id }, tr("act.sqMoved", { queue: queueName(x) })) })) : []),
    { label: t("card.details"), icon: "layers", onSelect: () => onOpen(d.id) },
    { label: t("card.remove"), icon: "trash", danger: true, onSelect: () => void actions.remove(d.id, false) },
  ];
  const up = canMove(order, byId, d.id, -1, q);
  const down = canMove(order, byId, d.id, 1, q);
  return (
    <li className={`card card-hover rise flex items-center gap-3 p-3 ${done ? "opacity-75" : ""}`} data-testid={`queue-${d.id}`} data-state={state}>
      <span className="num grid h-7 w-7 shrink-0 place-items-center rounded-lg bg-accent-soft text-xs font-semibold text-accent">{position}</span>
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 items-center gap-2">
          <button className="min-w-0 truncate text-start text-[13px] font-semibold hover:underline" onClick={() => onOpen(d.id)}>
            {d.filename}
          </button>
          <QueueStateChip state={state} />
        </div>
        <p className="truncate text-xs text-dim" data-testid="sq-why">
          {why ? reasonText(why, d, q) : state === "failed" ? (d.error ?? hostOf(d.url)) : hostOf(d.url)}
          {showBar && d.totalSize ? ` · ${percent(d.downloaded, d.totalSize)}%` : ""}
        </p>
        {showBar && <ProgressBar className="mt-1.5" value={frac} live={state === "downloading"} tone={state === "downloading" ? "accent" : state === "retrying" || state === "paused" ? "warn" : "dim"} label={d.filename} />}
      </div>
      <select
        aria-label={tr("q.prioFor", { name: d.filename })}
        className="input h-8 w-24 py-0"
        value={d.priority}
        disabled={done}
        onChange={(e) => void actions.setPriority(d.id, e.target.value as Priority)}
      >
        {prios.map((p) => (
          <option key={p} value={p}>
            {tr(`prio.${p}` as Key)}
          </option>
        ))}
      </select>
      <div className="flex shrink-0 flex-col">
        <button type="button" className="btn btn-ghost h-4 w-7 px-0" aria-label={t("sq.up")} title={t("sq.up")} disabled={!up} data-testid="sq-up" onClick={() => void actions.place({ id: d.id, step: "up" })}>
          <Icon name="chevronDown" size={13} className="rotate-180" />
        </button>
        <button type="button" className="btn btn-ghost h-4 w-7 px-0" aria-label={t("sq.down")} title={t("sq.down")} disabled={!down} data-testid="sq-down" onClick={() => void actions.place({ id: d.id, step: "down" })}>
          <Icon name="chevronDown" size={13} />
        </button>
      </div>
      <PrimaryAction d={d} compact />
      <Menu label={t("sq.rowMenu", { name: d.filename })} items={menu} />
    </li>
  );
});

// ------------------------------------------------------------------ start at

/** "Start this download at…": a date and time, with a few quick choices. */
export function StartAtDialog({ d, onClose }: { d: Download; onClose: () => void }) {
  const t = useT();
  const [at, setAt] = useState(d.startAt ?? localStamp(new Date(Date.now() + 3600_000)));
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    input.current?.focus();
    const esc = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("keydown", esc);
    return () => document.removeEventListener("keydown", esc);
  }, [onClose]);
  const past = at < localStamp(new Date());
  const save = () => {
    onClose();
    void actions.place({ id: d.id, start: { kind: "at", at } }, tr("act.sqScheduled", { name: d.filename, when: formatWhen(at) }));
  };
  return createPortal(
    <div className="fixed inset-0 z-50 grid place-items-center bg-black/50 p-4" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <div className="card rise w-full max-w-sm space-y-4 p-5 shadow-xl" role="dialog" aria-modal="true" aria-labelledby="sq-at-title" data-testid="sq-at-dialog">
        <div className="flex items-start gap-3">
          <div className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-accent-soft text-accent">
            <Icon name="calendar" size={19} />
          </div>
          <h2 id="sq-at-title" className="min-w-0 break-words text-[15px] font-semibold">
            {t("sq.atTitle", { name: d.filename })}
          </h2>
        </div>
        <div className="flex flex-wrap gap-1.5">
          {quickTimes(new Date()).map((qt) => (
            <button key={qt.key} type="button" className="pill" aria-pressed={at === qt.at} onClick={() => setAt(qt.at)}>
              {t(qt.key)}
            </button>
          ))}
        </div>
        <label className="block space-y-1.5">
          <span className="label">{t("sq.atLabel")}</span>
          <input ref={input} type="datetime-local" className="input" data-testid="sq-at-input" value={at} onChange={(e) => setAt(e.target.value.slice(0, 16))} />
        </label>
        {past && <p className="text-xs text-dim">{t("sq.atPast")}</p>}
        <div className="flex justify-end gap-2">
          <button className="btn" onClick={onClose}>
            {t("common.cancel")}
          </button>
          <button className="btn btn-primary" data-testid="sq-at-save" disabled={!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/.test(at)} onClick={save}>
            <Icon name="calendar" size={14} /> {t("sq.atSave")}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
