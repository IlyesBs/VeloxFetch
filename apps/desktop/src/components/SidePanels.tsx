import { useId } from "react";
import { FILL_LINK } from "./AddDownload";
import { formatBytes, formatSpeed } from "../format";
import { useLiveIo, useStore } from "../hooks";
import { useT } from "../i18n";
import { Icon, type IconName } from "../ui/icons";
import { canOpen } from "./FileIcon";

/**
 * Two real series over one scale: `main` filled (download / disk writes), `second` as a line (upload / disk
 * reads). One scale keeps them comparable: a small upload looks small next to a big download.
 */
function DualChart({ main, second, label, violetMain = false }: { main: number[]; second: number[]; label: string; violetMain?: boolean }) {
  const id = useId().replace(/:/g, "");
  const W = 100;
  const H = 34;
  const max = Math.max(1, ...main, ...second);
  const path = (vs: number[]) => vs.map((v, i) => `${i ? "L" : "M"}${(vs.length <= 1 ? W : (i / (vs.length - 1)) * W).toFixed(2)} ${(H - 1 - (v / max) * (H - 4)).toFixed(2)}`).join(" ");
  const live = path(main);
  const fill = violetMain ? "stop-violet" : "stop-accent";
  return (
    <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" role="img" aria-label={label} className="block h-20 w-full">
      <defs>
        <linearGradient id={`nf${id}`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" className={fill} stopOpacity={0.32} />
          <stop offset="1" className={fill} stopOpacity={0} />
        </linearGradient>
      </defs>
      {main.length > 1 && (
        <>
          <path d={`${live} L${W} ${H} L0 ${H} Z`} fill={`url(#nf${id})`} />
          <path d={path(second)} fill="none" className={violetMain ? "spark-line stroke-accent" : "stroke-violet"} strokeWidth={1.6} vectorEffect="non-scaling-stroke" strokeLinejoin="round" />
          <path d={live} fill="none" className={violetMain ? "stroke-violet" : "spark-line stroke-accent"} strokeWidth={2} vectorEffect="non-scaling-stroke" strokeLinejoin="round" />
        </>
      )}
    </svg>
  );
}

/** A measured speed, or a dash where the system gives no number. */
const rate = (v: number | null) => (v === null ? "—" : formatSpeed(v));

function QuickAction({ icon, label, testId, onClick }: { icon: IconName; label: string; testId: string; onClick: () => void }) {
  return (
    <button className="quick" data-testid={testId} onClick={onClick}>
      <span className="quick-icon">
        <Icon name={icon} size={21} />
      </span>
      <span className="max-w-full truncate">{label}</span>
    </button>
  );
}

/**
 * The dashboard's side column: live network and disk activity, what runs next, and quick actions. Only
 * real numbers, measured by the system: the whole computer's download and upload speed, the read and write
 * speed of the download folder's disk (a dash where the system gives none), the waiting downloads in queue order.
 */
export function SidePanels({ onOpen, onQueue, onBatch, onStream, onSettings }: { onOpen: (id: number) => void; onQueue: () => void; onBatch: () => void; onStream: (url: string) => void; onSettings: () => void }) {
  const t = useT();
  const m = useStore((s) => s.metrics);
  const io = useStore((s) => s.ioHistory);
  const queued = useStore((s) => s.groups.queued);
  const byId = useStore((s) => s.byId);
  useLiveIo();
  const next = queued.slice(0, 5).map((id) => byId[id]).filter((d) => d !== undefined);

  return (
    <>
      <section className="panel" data-testid="panel-network" aria-labelledby="pn-title">
        <h2 id="pn-title" className="text-[15px] font-semibold">
          {t("side.network")}
        </h2>
        <div className="mt-2 flex flex-wrap items-center gap-x-5 gap-y-1 text-[13px]">
          <span className="num ltr flex items-center gap-1.5 text-accent" title={t("side.download")} data-testid="net-down">
            <Icon name="arrowDown" size={14} />
            {rate(m.netDownBps)}
          </span>
          <span className="num ltr flex items-center gap-1.5 text-violet" title={t("side.upload")} data-testid="net-up">
            <Icon name="arrowDown" size={14} className="rotate-180" />
            {rate(m.netUpBps)}
          </span>
        </div>
        <div className="-mx-1 mt-2">
          <DualChart main={io.down} second={io.up} label={t("side.chart")} />
        </div>
      </section>

      <section className="panel" data-testid="panel-disk" aria-labelledby="pd-title">
        <h2 id="pd-title" className="text-[15px] font-semibold">
          {t("side.disk")}
        </h2>
        <div className="mt-2 flex flex-wrap items-center justify-between gap-x-4 gap-y-1 text-[13px] text-dim">
          <span className="num" data-testid="disk-write">
            {t("side.write")} <span className="ltr text-fg">{rate(m.diskWriteBps)}</span>
          </span>
          <span className="num" data-testid="disk-read" title={m.diskFree !== null ? `${t("side.free")} ${formatBytes(m.diskFree)}` : undefined}>
            {t("side.read")} <span className="ltr text-violet">{rate(m.diskReadBps)}</span>
          </span>
        </div>
        <div className="chart-grid -mx-1 mt-2 rounded-lg">
          <DualChart main={io.write} second={io.read} label={t("side.diskChart")} violetMain />
        </div>
      </section>

      <section className="panel" data-testid="panel-upcoming" aria-labelledby="pu-title">
        <div className="flex items-center justify-between gap-2">
          <h2 id="pu-title" className="text-[15px] font-semibold">
            {t("side.upcoming")} <span className="num text-dim">({queued.length})</span>
          </h2>
          <button className="tool-btn h-8 w-8" aria-label={t("side.openQueue")} title={t("side.openQueue")} onClick={onQueue}>
            <Icon name="more" size={18} />
          </button>
        </div>
        {next.length === 0 ? (
          <p className="mt-2 text-[13px] text-dim">{t("side.noUpcoming")}</p>
        ) : (
          <ul className="mt-2 space-y-0.5">
            {next.map((d) => (
              <li key={d.id}>
                <button className="flex w-full items-center gap-2.5 rounded-lg px-1.5 py-1.5 text-start text-[13px] hover:bg-accent-soft" onClick={() => onOpen(d.id)} title={d.filename}>
                  <Icon name={canOpen(d.filename, d.kind) ? "doc" : "archive"} size={15} className="shrink-0 text-dim" />
                  <span className="min-w-0 flex-1 truncate">{d.filename}</span>
                  <span className="num ltr shrink-0 text-dim">{d.totalSize !== null ? formatBytes(d.totalSize) : "—"}</span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="panel" data-testid="panel-quick" aria-labelledby="pq-title">
        <h2 id="pq-title" className="text-[15px] font-semibold">
          {t("side.quick")}
        </h2>
        <div className="mt-3 grid grid-cols-4 gap-2">
          <QuickAction icon="link" label={t("qa.link")} testId="qa-link" onClick={() => window.dispatchEvent(new CustomEvent(FILL_LINK, { detail: "" }))} />
          <QuickAction icon="layers" label={t("qa.several")} testId="qa-several" onClick={onBatch} />
          <QuickAction icon="film" label={t("qa.video")} testId="qa-video" onClick={() => onStream("")} />
          <QuickAction icon="gear" label={t("qa.settings")} testId="qa-settings" onClick={onSettings} />
        </div>
      </section>
    </>
  );
}
