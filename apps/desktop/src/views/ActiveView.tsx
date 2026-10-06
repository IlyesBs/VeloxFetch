import type { ReactNode } from "react";
import { actions } from "../actions";
import { DownloadCard } from "../components/DownloadCard";
import { MediaJobs } from "../components/MediaJobs";
import { formatBytes, formatEta, formatSpeed } from "../format";
import { store, useStore } from "../hooks";
import { useT } from "../i18n";
import type { State } from "../store";
import type { Download } from "../types";
import { Icon, type IconName } from "../ui/icons";
import { EmptyState, Sparkline } from "../ui/primitives";

/** Live totals over the running downloads, as one string so the strip only re-renders when a number changes. */
function live(s: State): string {
  let conns = 0;
  let left = 0;
  let unknown = false;
  for (const id of s.groups.active) {
    const d = s.byId[id];
    if (!d) continue;
    conns += d.connections;
    if (d.totalSize === null) unknown = true;
    else left += Math.max(0, d.totalSize - d.downloaded);
  }
  for (const j of s.media) if (j.status === "downloading") conns += 1;
  return JSON.stringify([conns, left, unknown]);
}

function Figure({ icon, label, value, testId }: { icon: IconName; label: string; value: string; testId: string }) {
  return (
    <div className="glass-tile flex min-w-0 items-center gap-3 px-4 py-3" data-testid={testId}>
      <Icon name={icon} size={22} strokeWidth={1.7} className="stat-icon shrink-0" />
      <div className="min-w-0">
        <div className="truncate text-[12px] text-dim">{label}</div>
        <div className="num display ltr truncate text-[19px] font-semibold leading-tight">{value}</div>
      </div>
    </div>
  );
}

function Heading({ title, n, right, testId }: { title: string; n: number; right?: ReactNode; testId?: string }) {
  return (
    <div className="flex items-center gap-2 px-1" data-testid={testId}>
      <h3 className="text-[14px] font-semibold">{title}</h3>
      <span className="num rounded-full bg-accent-soft px-2 py-px text-[11.5px] font-semibold text-accent">{n}</span>
      <div className="flex-1" />
      {right}
    </div>
  );
}

/**
 * Work in progress, and only that: what is downloading now (full cards with the connection panel), with
 * live totals on top (speed, connections in use, time left, what is still to come), then what waits for
 * a slot and what is paused. Finished files are on the Completed page; the dashboard keeps the overview.
 */
export function ActiveView({ onOpen, onQueue, onAdd }: { onOpen: (id: number) => void; onQueue: () => void; onAdd: () => void }) {
  const t = useT();
  const groups = useStore((s) => s.groups);
  const speed = useStore((s) => s.metrics.speedBps);
  const history = useStore((s) => s.speedHistory);
  const [conns, left, unknown] = JSON.parse(useStore(live)) as [number, number, boolean];
  const videos = useStore((s) => s.media.filter((j) => ["preparing", "downloading", "combining", "paused"].includes(j.status)).length);
  const list = (ids: number[]) => ids.map((id) => store.getState().byId[id]).filter((d): d is Download => !!d);
  const running = groups.active.length;
  const eta = speed > 0 && !unknown && left > 0 ? formatEta(Math.round(left / speed)) : "—";
  // Paused ones, plus the ones added with "Download later" that nothing else will start.
  const idleQueued = useStore((s) => s.groups.queued.filter((id) => s.byId[id]?.idle).join(","));
  const startable = [...groups.paused, ...(idleQueued ? idleQueued.split(",").map(Number) : [])];
  const nothing = running + groups.queued.length + groups.paused.length + videos === 0;

  return (
    <div className="space-y-5" data-testid="active-view">
      <header className="flex flex-wrap items-end gap-3 pt-2">
        <div className="min-w-0 flex-1">
          <h2 className="display text-[24px] font-semibold">{t("active.title")}</h2>
          <p className="text-[13px] text-dim">{t("active.sub", { n: running, w: groups.queued.length })}</p>
        </div>
        {running > 0 && (
          <button className="btn h-9" data-testid="active-pause-all" onClick={() => void actions.pauseAll(list(groups.active))}>
            <Icon name="pause" size={14} /> {t("active.pauseAll")}
          </button>
        )}
        {startable.length > 0 && (
          <button className="btn btn-primary h-9" data-testid="active-resume-all" onClick={() => void actions.resumeAll(list(startable))}>
            <Icon name="play" size={14} /> {t("active.resumeAll")}
          </button>
        )}
      </header>

      {/* the live strip: one wide speed chart and the numbers that matter while things run */}
      <div className="@container">
        <div className="grid gap-3.5 @[760px]:grid-cols-[1.6fr_1fr_1fr_1fr] grid-cols-2">
          <div className="glass-tile col-span-2 flex min-w-0 flex-col px-4 pt-3 @[760px]:col-span-1" data-testid="active-speed">
            <div className="flex items-baseline gap-2">
              <span className="text-[12px] text-dim">{t("active.speed")}</span>
              <span className="num display ltr ms-auto text-[22px] font-semibold text-accent">{formatSpeed(speed)}</span>
            </div>
            <Sparkline values={history} label={t("side.chart")} className="-mx-1 mt-1 h-12" />
          </div>
          <Figure icon="grid" label={t("active.conns")} value={String(conns)} testId="active-conns" />
          <Figure icon="clock" label={t("active.left")} value={eta} testId="active-eta" />
          <Figure icon="download" label={t("active.remaining")} value={unknown ? "—" : formatBytes(left)} testId="active-remaining" />
        </div>
      </div>

      {nothing && (
        <div className="space-y-3">
          <EmptyState icon="activeBox" title={t("active.empty")} body={t("active.emptyBody")} />
          <div className="text-center">
            <button className="btn btn-primary" data-testid="active-add" onClick={onAdd}>
              <Icon name="plus" size={14} /> {t("add.download")}
            </button>
          </div>
        </div>
      )}

      {running > 0 && (
        <section className="space-y-3">
          <Heading title={t("active.running")} n={running} />
          <div className="space-y-3" data-testid="active-list">
            {groups.active.map((id) => (
              <DownloadCard key={id} id={id} onOpen={onOpen} density="cards" />
            ))}
          </div>
        </section>
      )}

      {videos > 0 && <MediaJobs only="active" />}

      {groups.queued.length > 0 && (
        <section className="space-y-3">
          <Heading
            title={t("active.waiting")}
            n={groups.queued.length}
            right={
              <button className="btn h-8 text-xs" data-testid="active-open-queue" onClick={onQueue}>
                <Icon name="layers" size={13} /> {t("active.openQueue")}
              </button>
            }
          />
          <div className="space-y-2" data-testid="active-waiting">
            {groups.queued.map((id) => (
              <DownloadCard key={id} id={id} onOpen={onOpen} density="compact" />
            ))}
          </div>
        </section>
      )}

      {groups.paused.length > 0 && (
        <section className="space-y-3">
          <Heading title={t("active.paused")} n={groups.paused.length} />
          <div className="space-y-2" data-testid="active-paused">
            {groups.paused.map((id) => (
              <DownloadCard key={id} id={id} onOpen={onOpen} density="compact" />
            ))}
          </div>
        </section>
      )}
    </div>
  );
}
