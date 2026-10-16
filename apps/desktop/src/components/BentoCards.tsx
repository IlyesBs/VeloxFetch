import { useId, type ReactNode } from "react";
import { formatBytes, formatSpeed } from "../format";
import { useStore } from "../hooks";
import { useT } from "../i18n";
import type { State } from "../store";
import { Icon, type IconName } from "../ui/icons";
import { Sparkline } from "../ui/primitives";
import { MultiNetworkBadge } from "./MultiNetworkCard";

/** "12.3 MB/s" as a big number and a small unit. The text reads the same as formatSpeed's. */
function Amount({ text, testId }: { text: string; testId?: string }) {
  const i = text.lastIndexOf(" ");
  const value = i > 0 ? text.slice(0, i) : text;
  const unit = i > 0 ? text.slice(i) : "";
  return (
    <span data-testid={testId} className="ltr whitespace-nowrap">
      {value}
      <span className="text-[15px] font-medium text-fg/75">{unit}</span>
    </span>
  );
}

/** One card of the top row: a glowing icon, its name, the value, then a sub-line or a chart. */
function Stat({ icon, label, tone, testId, value, below, side }: { icon: IconName; label: string; tone?: "cyan" | "violet"; testId: string; value: ReactNode; below?: ReactNode; side?: ReactNode }) {
  return (
    <div className={`stat ${tone ? `stat-${tone}` : ""}`} data-testid={testId}>
      <div className="flex min-w-0 items-start gap-3.5">
        <Icon name={icon} size={30} strokeWidth={1.6} className="stat-icon mt-0.5 shrink-0" />
        <div className="min-w-0 flex-1">
          <div className="truncate text-[13px] font-medium text-fg/85">{label}</div>
          <div className="num display mt-1 truncate text-[25px] font-semibold leading-tight">{value}</div>
        </div>
        {side}
      </div>
      {below}
    </div>
  );
}

/** The used share of the disk as a gradient ring. */
function Ring({ value, bad }: { value: number; bad: boolean }) {
  const id = `ring-${useId().replace(/:/g, "")}`;
  const r = 24;
  const c = 2 * Math.PI * r;
  const v = Math.min(1, Math.max(0, value));
  return (
    <svg viewBox="0 0 60 60" className="h-[60px] w-[60px] shrink-0 -rotate-90" aria-hidden="true">
      <defs>
        <linearGradient id={id} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" className="stop-accent" />
          <stop offset="1" className="stop-violet" />
        </linearGradient>
      </defs>
      <circle cx="30" cy="30" r={r} fill="none" className="gauge-track" strokeWidth={5} />
      <circle cx="30" cy="30" r={r} fill="none" stroke={bad ? undefined : `url(#${id})`} className={bad ? "stroke-bad" : "spark-line"} strokeWidth={5} strokeLinecap="round" style={{ strokeDasharray: `${(c * v).toFixed(2)} ${c.toFixed(2)}` }} />
    </svg>
  );
}

/** "2.1 GB / 12.6 GB" over the given downloads (the ones of unknown size count what they have). */
function sizes(s: State, ids: number[]): string {
  let done = 0;
  let total = 0;
  for (const id of ids) {
    const d = s.byId[id];
    if (!d) continue;
    done += d.downloaded;
    total += d.totalSize ?? d.downloaded;
  }
  return `${formatBytes(done)} / ${formatBytes(total)}`;
}

/**
 * The dashboard's top row of six cards. Reads the aggregated metrics that arrive ~5 times a second. Laid
 * out on the width it actually gets (a container query), not the window's: 2, 3 or 6 columns.
 */
export function BentoCards() {
  const t = useT();
  const m = useStore((s) => s.metrics);
  const history = useStore((s) => s.speedHistory);
  // strings, so these only re-render the row when the text changes
  const activeSizes = useStore((s) => sizes(s, s.groups.active));
  const queuedSize = useStore((s) => {
    let n = 0;
    for (const id of s.groups.queued) n += s.byId[id]?.totalSize ?? 0;
    return formatBytes(n);
  });

  const used = m.diskTotal !== null && m.diskFree !== null ? m.diskTotal - m.diskFree : null;
  const usedFrac = used !== null && m.diskTotal ? used / m.diskTotal : 0;
  // the trend of the last two seconds against the two before
  const recent = history.slice(-10);
  const a = recent.slice(0, 5).reduce((x, y) => x + y, 0);
  const b = recent.slice(5).reduce((x, y) => x + y, 0);
  const trend = m.speedBps > 0 && recent.length === 10 ? (b > a * 1.05 ? "up" : b < a * 0.95 ? "down" : null) : null;

  return (
    <div className="@container">
      <div className="grid grid-cols-2 gap-3.5 @[620px]:grid-cols-3 @[1080px]:grid-cols-[1fr_1.1fr_0.72fr_1.1fr_1.05fr_1.25fr]" data-testid="bento">
        <Stat
          icon="gauge"
          label={t("stat.speed")}
          tone="cyan"
          testId="tile-speed"
          side={<MultiNetworkBadge />}
          value={
            <span className="inline-flex items-center gap-1.5">
              <Amount text={formatSpeed(m.speedBps)} testId="speed-value" />
              {trend && <Icon name={trend === "up" ? "trendUp" : "trendDown"} size={16} className={trend === "up" ? "text-accent" : "text-dim"} aria-label={trend === "up" ? t("stat.rising") : t("stat.falling")} role="img" />}
            </span>
          }
          below={<Sparkline values={history} label={t("bento.chart")} className="-mx-1 mt-auto h-9 pt-2" />}
        />
        <Stat
          icon="inbox"
          label={t("stat.active")}
          testId="tile-active"
          value={<span data-testid="active-value">{m.active}</span>}
          below={<div className="num ltr mt-auto truncate pt-2 text-start text-[12.5px] text-dim">{activeSizes}</div>}
        />
        <Stat
          icon="layers"
          label={t("stat.queue")}
          testId="tile-queued"
          value={<span data-testid="queued-value">{m.queued}</span>}
          below={<div className="num mt-auto truncate pt-2 text-[12.5px] text-dim">{m.queued > 0 ? <span className="ltr">{queuedSize}</span> : t("bento.queuedSub")}</div>}
        />
        <Stat
          icon="disk"
          label={t("stat.total")}
          testId="tile-total"
          value={<Amount text={formatBytes(m.totalDownloaded)} />}
          below={
            history.some((v) => v > 0) ? (
              <Sparkline values={history} label={t("bento.chart")} className="-mx-1 mt-auto h-9 pt-2" />
            ) : (
              <div className="mt-auto truncate pt-2 text-[12.5px] text-dim">{t("bento.totalSub", { n: m.completed })}</div>
            )
          }
        />
        <Stat
          icon="network"
          label={t("stat.network")}
          tone="violet"
          testId="tile-network"
          value={<Amount text={formatBytes(m.networkTotal)} testId="network-total" />}
          below={
            <dl className="mt-auto space-y-0.5 pt-2 text-[12.5px] leading-[17px]" data-testid="network-period" title={t("bento.networkTip", { session: formatBytes(m.sessionDownloaded) })}>
              {([
                ["today", t("bento.networkToday"), m.networkToday],
                ["month", t("bento.networkMonth"), m.networkMonth],
              ] as const).map(([k, label, bytes]) => (
                <div key={k} className="flex min-w-0 items-baseline justify-between gap-2">
                  <dt className="min-w-0 truncate text-dim">{label}</dt>
                  <dd className="num ltr shrink-0 whitespace-nowrap text-fg/85" data-testid={`network-${k}`}>
                    {formatBytes(bytes)}
                  </dd>
                </div>
              ))}
            </dl>
          }
        />
        <Stat
          icon="hdd"
          label={t("stat.disk")}
          testId="tile-disk"
          value={used !== null ? `${Math.round(usedFrac * 100)}%` : "—"}
          side={
            <div role="progressbar" aria-label={t("bento.diskUsed")} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(usedFrac * 100)}>
              <Ring value={usedFrac} bad={usedFrac > 0.9} />
            </div>
          }
          below={
            <div className="num mt-auto truncate pt-2 text-[12.5px] text-dim" title={m.diskTotal !== null ? t("bento.diskSub", { free: formatBytes(m.diskFree), total: formatBytes(m.diskTotal), folder: formatBytes(m.folderBytes) }) : undefined}>
              {m.diskTotal !== null && used !== null ? <span className="ltr">{t("stat.diskUsed", { used: formatBytes(used), total: formatBytes(m.diskTotal) })}</span> : t("bento.reading")}
            </div>
          }
        />
      </div>
    </div>
  );
}
