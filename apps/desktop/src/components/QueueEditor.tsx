import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { actions } from "../actions";
import { formatBytes, formatSpeed } from "../format";
import { t as tr, useT, type Key } from "../i18n";
import { MAIN_QUEUE, NO_RULES, queueName } from "../smartQueue";
import type { ConnectionRule, QueueInfo, QueueRules, QueueWhen } from "../types";
import { Icon, type IconName } from "../ui/icons";

const DAYS: Key[] = ["day.0", "day.1", "day.2", "day.3", "day.4", "day.5", "day.6"];
const TIME = /^([01]\d|2[0-3]):[0-5]\d$/;
const KB = 1024;
const MB = 1024 * 1024;
const GB = 1024 * MB;
/** "Pause while the connection is busy above…": a few clear steps. */
const BUSY = [256 * KB, 512 * KB, 1 * MB, 2 * MB, 5 * MB, 10 * MB, 20 * MB];
/** "Big files go here": larger than… */
const BIG = [100 * MB, 500 * MB, 1 * GB, 2 * GB, 5 * GB];
const CONNECTIONS: ConnectionRule[] = ["any", "wifi", "wired", "notMobile"];
const ALL_DAYS = [true, true, true, true, true, true, true];

/** Ready-made setups for the common wishes, so most people never fill the form themselves. */
const PRESETS: { key: Key; icon: IconName; name: Key; rules: QueueRules }[] = [
  {
    key: "sq.presetNight",
    icon: "moon",
    name: "sq.presetNightName",
    rules: { ...NO_RULES, when: { kind: "between", start: "23:00", stop: "07:00", days: ALL_DAYS }, largeFilesOver: 500 * MB },
  },
  { key: "sq.presetAway", icon: "leaf", name: "sq.presetAwayName", rules: { ...NO_RULES, onlyWhenIdle: true } },
  { key: "sq.presetTwo", icon: "clock", name: "sq.presetTwoName", rules: { ...NO_RULES, when: { kind: "startAt", time: "02:00", days: ALL_DAYS } } },
  { key: "sq.presetWifi", icon: "network", name: "sq.presetWifiName", rules: { ...NO_RULES, connection: "notMobile", pauseAboveBps: 2 * MB } },
];

/** Why the form can't be saved, or null. The backend checks again. */
export function queueProblem(name: string, rules: QueueRules, main: boolean): string | null {
  if (!main && !name.trim()) return tr("sq.needName");
  if (name.trim().length > 40) return tr("sq.nameLong");
  const w = rules.when;
  if (w.kind === "between") {
    if (!TIME.test(w.start) || !TIME.test(w.stop)) return tr("sched.badTime");
    if (w.start === w.stop) return tr("sched.same");
  }
  if (w.kind === "startAt" && !TIME.test(w.time)) return tr("sched.badTime");
  if (w.kind !== "anytime" && !w.days.some(Boolean)) return tr("sched.noDay");
  return null;
}

/** Create or change a queue: a name, when it may download, and a few plain conditions. */
export function QueueEditor({ queue, idleKnown, onClose, onSaved }: { queue: QueueInfo | null; idleKnown: boolean; onClose: () => void; onSaved?: (id: number) => void }) {
  const t = useT();
  const main = queue?.id === MAIN_QUEUE;
  const [name, setName] = useState(queue && !main ? queue.name : "");
  const [rules, setRules] = useState<QueueRules>(queue?.rules ?? NO_RULES);
  const first = useRef<HTMLInputElement>(null);
  useEffect(() => {
    first.current?.focus();
    const esc = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("keydown", esc);
    return () => document.removeEventListener("keydown", esc);
  }, [onClose]);

  const edit = (patch: Partial<QueueRules>) => setRules({ ...rules, ...patch });
  const setWhen = (kind: QueueWhen["kind"]) => {
    const days = rules.when.kind === "anytime" ? ALL_DAYS : rules.when.days;
    if (kind === "anytime") edit({ when: { kind } });
    if (kind === "between") edit({ when: { kind, start: rules.when.kind === "startAt" ? rules.when.time : "23:00", stop: "07:00", days } });
    if (kind === "startAt") edit({ when: { kind, time: rules.when.kind === "between" ? rules.when.start : "02:00", days } });
  };
  const w = rules.when;
  const problem = queueProblem(name, rules, main);

  const save = async () => {
    const res = await actions.saveQueue({ id: queue?.id ?? null, name: main ? "" : name.trim(), rules }, queue ? tr("act.sqSaved") : tr("act.sqCreated"));
    if (!res) return;
    onClose();
    const id = queue?.id ?? res.queues[res.queues.length - 1]?.id;
    if (id !== undefined) onSaved?.(id);
  };
  const remove = async () => {
    if (!queue || main) return;
    if (await actions.deleteQueue(queue.id)) onClose();
  };

  const whenBtn = (kind: QueueWhen["kind"], icon: IconName, label: Key) => (
    <button type="button" role="radio" aria-checked={w.kind === kind} className="mode-btn" data-testid={`sq-when-${kind}`} onClick={() => setWhen(kind)}>
      <Icon name={icon} size={17} />
      {t(label)}
    </button>
  );

  return createPortal(
    <div className="fixed inset-0 z-50 grid place-items-center overflow-y-auto bg-black/50 p-4" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <div className="card rise w-full max-w-lg space-y-4 p-5 shadow-xl" role="dialog" aria-modal="true" aria-labelledby="sq-edit-title" data-testid="sq-editor">
        <div className="flex items-start gap-3">
          <div className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-accent-soft text-accent">
            <Icon name="queue" size={19} />
          </div>
          <div className="min-w-0">
            <h2 id="sq-edit-title" className="text-[15px] font-semibold">
              {queue ? t("sq.editTitle", { name: queueName(queue) }) : t("sq.newTitle")}
            </h2>
            <p className="mt-0.5 text-xs text-dim">{t("sq.editHint")}</p>
          </div>
        </div>

        {!queue && (
          <div className="space-y-1.5">
            <span className="label">{t("sq.presets")}</span>
            <div className="flex flex-wrap gap-1.5">
              {PRESETS.map((p) => (
                <button
                  key={p.key}
                  type="button"
                  className="pill"
                  data-testid={`sq-preset-${p.key.slice(9).toLowerCase()}`}
                  onClick={() => {
                    setRules(p.rules);
                    if (!name.trim()) setName(t(p.name));
                  }}
                >
                  <Icon name={p.icon} size={13} /> {t(p.key)}
                </button>
              ))}
            </div>
          </div>
        )}

        {!main && (
          <label className="block space-y-1.5">
            <span className="label">{t("sq.name")}</span>
            <input ref={first} className="input" data-testid="sq-name" value={name} maxLength={40} placeholder={t("sq.namePh")} onChange={(e) => setName(e.target.value)} />
          </label>
        )}

        <div className="space-y-2">
          <span className="label">{t("sq.when")}</span>
          <div role="radiogroup" aria-label={t("sq.when")} className="flex gap-2">
            {whenBtn("anytime", "bolt", "sq.whenAny")}
            {whenBtn("between", "moon", "sq.whenBetween")}
            {whenBtn("startAt", "clock", "sq.whenStartAt")}
          </div>
          {w.kind !== "anytime" && (
            <div className="rise space-y-2 rounded-xl border border-line p-3">
              <div className="flex flex-wrap items-end gap-3">
                {w.kind === "between" ? (
                  <>
                    <label className="block text-xs text-dim">
                      {t("sq.from")}
                      <input type="time" className="input mt-1 w-28" data-testid="sq-from" value={w.start} onChange={(e) => edit({ when: { ...w, start: e.target.value } })} />
                    </label>
                    <label className="block text-xs text-dim">
                      {t("sq.to")}
                      <input type="time" className="input mt-1 w-28" data-testid="sq-to" value={w.stop} onChange={(e) => edit({ when: { ...w, stop: e.target.value } })} />
                    </label>
                  </>
                ) : (
                  <label className="block text-xs text-dim">
                    {t("sq.at")}
                    <input type="time" className="input mt-1 w-28" data-testid="sq-at" value={w.time} onChange={(e) => edit({ when: { ...w, time: e.target.value } })} />
                  </label>
                )}
                <div role="group" aria-label={t("sched.days")} className="flex flex-wrap gap-1">
                  {DAYS.map((d, i) => (
                    <button
                      key={d}
                      type="button"
                      aria-pressed={w.days[i]}
                      onClick={() => edit({ when: { ...w, days: w.days.map((on, j) => (j === i ? !on : on)) } })}
                      className={`h-8 rounded-lg px-2 text-xs font-medium transition-colors duration-150 ${w.days[i] ? "bg-accent-soft text-accent" : "border border-line text-dim hover:text-fg"}`}
                    >
                      {t(d)}
                    </button>
                  ))}
                </div>
              </div>
              <p className="text-xs text-dim">{t(w.kind === "between" ? "sq.whenHowBetween" : "sq.whenHowStartAt")}</p>
            </div>
          )}
        </div>

        <div className="space-y-1">
          <label className="flex cursor-pointer items-center gap-2 text-[13px]">
            <input type="checkbox" data-testid="sq-idle" checked={rules.onlyWhenIdle} onChange={(e) => edit({ onlyWhenIdle: e.target.checked })} />
            {t("sq.idle")}
          </label>
          <p className="ms-6 text-xs text-dim">{idleKnown ? t("sq.idleHint") : t("sq.idleUnknown")}</p>
        </div>

        <div className="grid gap-3 sm:grid-cols-3">
          <label className="space-y-1">
            <span className="label">{t("sq.connection")}</span>
            <select className="input h-9" data-testid="sq-connection" value={rules.connection} onChange={(e) => edit({ connection: e.target.value as ConnectionRule })}>
              {CONNECTIONS.map((c) => (
                <option key={c} value={c}>
                  {t(`sq.conn.${c}` as Key)}
                </option>
              ))}
            </select>
          </label>
          <label className="space-y-1">
            <span className="label">{t("sq.busy")}</span>
            <select className="input h-9" data-testid="sq-busy" value={rules.pauseAboveBps ?? 0} onChange={(e) => edit({ pauseAboveBps: Number(e.target.value) || null })}>
              <option value={0}>{t("sq.never")}</option>
              {(rules.pauseAboveBps && !BUSY.includes(rules.pauseAboveBps) ? [...BUSY, rules.pauseAboveBps].sort((a, b) => a - b) : BUSY).map((v) => (
                <option key={v} value={v}>
                  {t("sq.above", { speed: formatSpeed(v) })}
                </option>
              ))}
            </select>
          </label>
          <label className="space-y-1">
            <span className="label">{t("sq.big")}</span>
            <select className="input h-9" data-testid="sq-big" value={rules.largeFilesOver ?? 0} onChange={(e) => edit({ largeFilesOver: Number(e.target.value) || null })}>
              <option value={0}>{t("sq.off")}</option>
              {(rules.largeFilesOver && !BIG.includes(rules.largeFilesOver) ? [...BIG, rules.largeFilesOver].sort((a, b) => a - b) : BIG).map((v) => (
                <option key={v} value={v}>
                  {t("sq.larger", { size: formatBytes(v) })}
                </option>
              ))}
            </select>
          </label>
        </div>
        <p className="text-xs text-dim">{t("sq.rulesHint")}</p>

        {problem && (
          <p className="text-xs text-bad" role="alert">
            {problem}
          </p>
        )}
        <div className="flex flex-wrap items-center gap-2">
          {queue && !main && (
            <button className="btn btn-ghost text-bad" data-testid="sq-delete" title={t("sq.deleteHint")} onClick={() => void remove()}>
              <Icon name="trash" size={14} /> {t("sq.delete")}
            </button>
          )}
          <span className="grow" />
          <button className="btn" onClick={onClose}>
            {t("common.cancel")}
          </button>
          <button className="btn btn-primary" data-testid="sq-save" disabled={problem !== null} onClick={() => void save()}>
            <Icon name="check" size={14} /> {t("sq.save")}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
