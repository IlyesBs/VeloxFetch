import { getLang, t as tr, useT, type Key } from "../i18n";
import { localError } from "../errors";
import { useEffect, useState, type ReactNode } from "react";
import { actions } from "../actions";
import { PrimaryAction } from "../components/DownloadCard";
import { DiagnosticsCard } from "../components/DiagnosticsCard";
import { DoctorPanel } from "../components/DoctorPanel";
import { openNewLink } from "../newLink";
import { buttons as doctorButtons, headline, retryLine, sourceLine } from "../doctor";
import { api } from "../api";
import { PreviewCard } from "../components/PreviewCard";
import { FileIcon, kindLabel } from "../components/FileIcon";
import { SignInDialog } from "../components/SignInDialog";
import { SegmentMap, SegmentRows } from "../components/SegmentMonitor";
import { formatBytes, formatEta, formatPercent, formatSpeed, fraction, parseRate } from "../format";
import { useDetail, useDownload } from "../hooks";
import { PRIORITIES, type Diagnosis, type Download, type Priority } from "../types";
import { Icon } from "../ui/icons";
import { ProgressBar, Sparkline, StatusChip } from "../ui/primitives";
import { verifyText } from "../components/VerifyBadge";

/** No diagnosis (a download that failed before Download Doctor): the Retry button keeps its usual label. */
const NO_DOCTOR: Diagnosis = { category: "unknown", code: "other", source: "unknown", httpStatus: null, retry: "unknown", retryAfterSecs: null, retryAtMs: null, severity: "recoverable", factors: [], actions: [], neededBytes: null, freeBytes: null };

export function Stat({ label, value, testId }: { label: string; value: ReactNode; testId?: string }) {
  return (
    <div className="rounded-xl border border-line bg-raised px-3.5 py-3">
      <div className="label">{label}</div>
      <div className="num mt-1 text-lg font-semibold leading-tight" data-testid={testId}>
        {value}
      </div>
    </div>
  );
}

export function Card({ title, right, children, testId }: { title: string; right?: ReactNode; children: ReactNode; testId?: string }) {
  return (
    <section className="card p-4" data-testid={testId}>
      <div className="mb-3 flex items-center justify-between">
        <h2 className="text-[13px] font-semibold">{title}</h2>
        {right}
      </div>
      {children}
    </section>
  );
}

/**
 * The file's integrity (feature #6), in plain words: verified against the publisher's checksum (which list, which
 * kind) or the user's; checked before it is finished; arrived damaged; or no checksum known (only its size is checked).
 */
function IntegrityCard({ d, checksum }: { d: Download; checksum: string | null }) {
  const t = useT();
  const v = d.verify;
  const damaged = d.errorCode === "publisher_checksum_mismatch" || d.errorCode === "checksum_mismatch";
  const [tone, title, text] = damaged
    ? (["text-bad", t("verify.damaged"), t(d.errorCode === "publisher_checksum_mismatch" ? "verify.damagedPublisherTip" : "verify.damagedYouTip")] as const)
    : v
      ? ([v.state === "verified" ? "text-ok" : v.state === "ready" ? "text-accent" : "text-dim", t(v.state === "verified" ? "verify.verified" : v.state === "ready" ? "verify.ready" : "verify.looking"), verifyText(t, v)] as const)
      : (["text-dim", t("verify.none"), t("verify.noneTip")] as const);
  return (
    <Card title={t("verify.title")} testId="integrity-card">
      <div className="flex items-start gap-3" data-state={damaged ? "damaged" : (v?.state ?? "none")} data-by={v?.by ?? undefined}>
        <Icon name={damaged ? "alert" : "shield"} size={30} strokeWidth={1.6} className={`mt-0.5 shrink-0 ${tone}`} />
        <div className="min-w-0 flex-1 space-y-1">
          <p className={`text-[15px] font-semibold ${tone}`} data-testid="integrity-title">
            {title}
          </p>
          <p className="text-[13px] text-dim" data-testid="integrity-text">
            {text}
          </p>
          {checksum && v?.state === "verified" && (
            <p className="flex min-w-0 items-center gap-2 pt-1">
              <span className="truncate font-mono text-xs text-fg/80" title={checksum} data-testid="integrity-checksum">
                {checksum}
              </span>
              <button className="shrink-0 text-dim hover:text-fg" aria-label={tr("det.copy", { what: t("det.checksum") })} onClick={() => void actions.copy(checksum)}>
                <Icon name="copy" size={14} />
              </button>
            </p>
          )}
        </div>
      </div>
    </Card>
  );
}

export function Meta({ k, v, mono = false, copy }: { k: string; v: string | null | undefined; mono?: boolean; copy?: boolean }) {
  return (
    <div className="grid grid-cols-[7rem_1fr_auto] items-baseline gap-3 py-1.5">
      <dt className="text-xs text-dim">{k}</dt>
      <dd className={`min-w-0 break-all ${mono ? "font-mono text-xs" : "text-[13px]"}`}>{v ?? "—"}</dd>
      {copy && v ? (
        <button className="text-dim hover:text-fg" aria-label={tr("det.copy", { what: k })} onClick={() => void actions.copy(v)}>
          <Icon name="copy" size={14} />
        </button>
      ) : (
        <span />
      )}
    </div>
  );
}

export function DetailView({ id, onBack }: { id: number; onBack: () => void }) {
  const t = useT();
  const live = useDownload(id);
  const { detail, speeds, error } = useDetail(id);
  const [limit, setLimit] = useState("");
  const [signIn, setSignIn] = useState(false);

  // The download was removed while its view was open.
  useEffect(() => {
    if (live === undefined && detail !== null) onBack();
  }, [live, detail, onBack]);

  if (!detail) {
    return (
      <div className="space-y-4">
        <button className="btn btn-ghost -ms-2" onClick={onBack}>
          <Icon name="back" className="flip-rtl" /> {t("det.back")}
        </button>
        <p className="text-dim">{error ?? t("common.loading")}</p>
      </div>
    );
  }

  const d = live ?? detail.download;
  const frac = fraction(d.downloaded, d.totalSize);
  const complete = detail.segments.filter((s) => s.status === "completed").length;
  const peak = Math.max(0, ...speeds);

  const applyLimit = () => {
    const bps = limit.trim() ? parseRate(limit) : null;
    if (limit.trim() && bps === null) return;
    void actions.setDownloadLimit(id, bps);
    setLimit("");
  };

  return (
    <div className="space-y-4" data-testid="detail-view">
      {signIn && <SignInDialog download={d} onClose={() => setSignIn(false)} />}
      <div className="flex items-center gap-3">
        <button className="btn btn-ghost btn-icon -ms-2" onClick={onBack} aria-label={t("det.backTo")} data-testid="detail-back">
          <Icon name="back" size={18} className="flip-rtl" />
        </button>
        <FileIcon kind={d.kind} />
        <div className="min-w-0 flex-1">
          <h1 className="truncate text-lg font-semibold tracking-tight" data-testid="detail-title">
            {d.filename}
          </h1>
          <p className="truncate text-xs text-dim">{d.url}</p>
        </div>
        <StatusChip status={d.status} />
        {d.status !== "cancelled" && (
          <button className="btn" data-testid="detail-show" onClick={() => void actions.showInFolder({ kind: "download", id })}>
            <Icon name="folder" size={15} /> {t("mj.showFolder")}
          </button>
        )}
        <PrimaryAction d={d} onSignIn={() => setSignIn(true)} />
        {!["completed", "cancelled"].includes(d.status) && (
          <button className="btn" onClick={() => void actions.cancel(id)}>
            <Icon name="x" size={15} /> {t("common.cancel")}
          </button>
        )}
      </div>

      {d.doctor && d.status === "downloading" && (
        <section className="card flex items-start gap-3 border-warn/40 p-3 text-xs" role="status" data-testid="doctor-live">
          <Icon name="retry" size={15} className="mt-0.5 shrink-0 text-warn" />
          <p>
            <span className="font-semibold">{t(headline(d.doctor).key, headline(d.doctor).vars)}</span> · {t(sourceLine(d.doctor).key)} ·{" "}
            {t(retryLine(d.doctor, Date.now()).key, retryLine(d.doctor, Date.now()).vars)}
          </p>
        </section>
      )}

      {d.error && (
        <section className="card flex items-start gap-3 border-bad/40 p-4" role="alert" data-testid="detail-error">
          <Icon name="alert" size={18} className="mt-0.5 shrink-0 text-bad" />
          <div className="min-w-0 flex-1 space-y-1">
            <p className="text-[13px] font-medium">{localError(d.errorCode, d.error)}</p>
            {d.errorDetail && d.errorDetail !== d.error && (
              <details className="text-xs text-dim">
                <summary className="cursor-pointer select-none">{t("det.tech")}</summary>
                <p className="selectable mt-1 break-all font-mono">{d.errorDetail}</p>
                {d.errorCode && <p className="mt-1 font-mono" data-testid="detail-error-code">{t("det.errCode", { code: d.errorCode })}</p>}
              </details>
            )}
            {d.doctor && d.status === "failed" && <DoctorPanel d={d.doctor} onNewLink={() => openNewLink(d.id)} />}
          </div>
          {d.status === "failed" && (
            <div className="flex shrink-0 gap-2">
              {d.needs === "signIn" && (
                <button className="btn btn-primary" onClick={() => setSignIn(true)}>
                  <Icon name="lock" size={15} /> {t("det.signIn")}
                </button>
              )}
              {d.doctor && doctorButtons(d.doctor).some((b) => b.action === "openFolder") && (
                <button className="btn" data-testid="doctor-open-folder" onClick={() => void api.openDownloadFolder().catch(() => {})}>
                  <Icon name="folder" size={15} /> {t("doc.btn.openFolder")}
                </button>
              )}
              <button className="btn" data-testid="detail-retry" onClick={() => void actions.retry(id)}>
                <Icon name="retry" size={15} /> {t(doctorButtons(d.doctor ?? NO_DOCTOR).find((b) => b.action === "retry")?.label ?? "mj.retry")}
              </button>
            </div>
          )}
        </section>
      )}

      <Card title={t("det.overview")}>
        <div className="mb-3 flex items-end justify-between">
          <span className="num text-3xl font-semibold leading-none tracking-tight" data-testid="detail-percent">
            {d.totalSize ? formatPercent(d.downloaded, d.totalSize) : "—"}
          </span>
          <span className="num text-xs text-dim">
            {formatBytes(d.downloaded)}
            {d.totalSize !== null ? ` of ${formatBytes(d.totalSize)}` : ""}
          </span>
        </div>
        <ProgressBar value={d.status === "completed" ? 1 : frac} label={t("det.overall")} tone={d.status === "completed" ? "ok" : "accent"} className="h-2" />
        <div className="mt-4 grid grid-cols-2 gap-2.5 sm:grid-cols-3 lg:grid-cols-6">
          <Stat label={t("det.progress")} value={t("det.chunks", { done: complete, total: detail.segments.length })} />
          <Stat label={t("det.speed")} value={formatSpeed(d.speedBps)} testId="detail-speed" />
          <Stat label={t("det.eta")} value={formatEta(d.etaSecs)} />
          <Stat label={t("dc.conn")} value={d.connections} />
          <Stat label={t("det.downloaded")} value={formatBytes(d.downloaded)} />
          <Stat label={t("det.remaining")} value={formatBytes(detail.remaining)} />
        </div>
      </Card>

      <div className="grid gap-4 lg:grid-cols-2">
        <Card title={t("det.network")}>
          <Sparkline values={speeds} label={t("det.throughput")} className="h-16" />
          <dl className="mt-3 divide-y divide-line">
            <Meta k={t("det.current")} v={formatSpeed(d.speedBps)} />
            <Meta k={t("det.average")} v={detail.avgSpeedBps > 0 ? formatSpeed(detail.avgSpeedBps) : "—"} />
            <Meta k={t("det.peak")} v={peak > 0 ? formatSpeed(peak) : "—"} />
            <Meta k={t("dc.conn")} v={t("det.connVal", { n: d.connections, mode: detail.mode })} />
            <Meta k={t("det.retries")} v={String(detail.retries)} />
          </dl>
        </Card>

        <Card title={t("det.controls")}>
          <div className="space-y-3">
            <label className="block space-y-1.5">
              <span className="label">{t("addd.priority")}</span>
              <select
                className="input"
                value={d.priority}
                disabled={["completed", "cancelled"].includes(d.status)}
                onChange={(e) => void actions.setPriority(id, e.target.value as Priority)}
                data-testid="detail-priority"
              >
                {PRIORITIES.map((p) => (
                  <option key={p} value={p}>
                    {t(`prio.${p}` as Key)}
                  </option>
                ))}
              </select>
            </label>
            <div className="space-y-1.5">
              <span className="label">{t("det.limitThis")}</span>
              <div className="flex gap-2">
                <input
                  className="input"
                  aria-label={t("det.limitThis")}
                  placeholder={d.speedLimit ? formatSpeed(d.speedLimit) : t("addd.limitEg")}
                  value={limit}
                  onChange={(e) => setLimit(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && applyLimit()}
                />
                <button className="btn" onClick={applyLimit}>
                  {t("common.set")}
                </button>
                <button className="btn" disabled={!d.speedLimit} onClick={() => void actions.setDownloadLimit(id, null)}>
                  {t("det.clear")}
                </button>
              </div>
            </div>
          </div>
        </Card>
      </div>

      <IntegrityCard d={d} checksum={detail.checksum} />

      <Card
        title={t("det.segments")}
        testId="segments-card"
        right={
          <span className="num text-xs text-dim">
            {t("det.nComplete", { done: complete, total: detail.segments.length })}
          </span>
        }
      >
        {detail.segments.length === 0 ? (
          <p className="text-dim">
            {detail.mode === "sequential" ? t("det.sequential") : t("det.noChunks")}
          </p>
        ) : (
          <div className="space-y-3">
            <SegmentMap segments={detail.segments} />
            <SegmentRows segments={detail.segments} />
          </div>
        )}
      </Card>

      {(d.kind === "video" || d.kind === "audio") && <PreviewCard id={id} status={d.status} />}

      <DiagnosticsCard id={id} status={d.status} revision={d} />

      <Card title={t("det.metadata")}>
        <dl className="divide-y divide-line">
          <Meta k={t("det.url")} v={d.url} copy />
          <Meta k={t("det.server")} v={detail.server} />
          <Meta k={t("det.kind")} v={kindLabel(d.kind)} />
          <Meta k={t("det.ctype")} v={detail.contentType} />
          <Meta k={t("det.size")} v={d.totalSize !== null ? t("det.bytes", { size: formatBytes(d.totalSize), n: d.totalSize.toLocaleString(getLang()) }) : null} />
          <Meta k={t("det.etag")} v={detail.etag} mono />
          <Meta k={t("det.modified")} v={detail.lastModified} />
          <Meta k={t("det.checksum")} v={detail.checksum} mono />
          <Meta k={t("det.savedTo")} v={d.destination} copy />
        </dl>
      </Card>
    </div>
  );
}
