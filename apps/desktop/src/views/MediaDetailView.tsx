import { useEffect, useRef, useState } from "react";
import { localNote, MediaControls, MediaPlayer, mediaLabel, mediaTone } from "../components/MediaJobs";
import { formatBytes, formatDuration, formatEta, formatSpeed } from "../format";
import { useStore } from "../hooks";
import { getLang, useT } from "../i18n";
import { averageBps, elapsedMs, qualityText, savedPaths, sizeOf, typeKey } from "../mediaDetail";
import { loadPreviewBase } from "../preview";
import { Icon } from "../ui/icons";
import { ProgressBar, Sparkline } from "../ui/primitives";
import { Card, Meta, Stat } from "./DetailView";

/** The stream's pieces as a strip of cells (at most 200: a cell stands for several pieces in a long stream). */
function PieceMap({ done, total }: { done: number; total: number }) {
  const cells = Math.min(200, total);
  const per = total / cells;
  return (
    <div className="flex flex-wrap gap-[3px]" role="img" aria-label={`${done} / ${total}`} data-testid="media-piece-map">
      {Array.from({ length: cells }, (_, i) => {
        const end = Math.round((i + 1) * per);
        return <span key={i} className={`h-3 w-3 rounded-[3px] ${end <= done ? "bg-ok" : Math.round(i * per) < done ? "bg-accent" : "bg-raised ring-1 ring-line"}`} />;
      })}
    </div>
  );
}

const when = (ms: number | null | undefined) => (ms ? new Date(ms).toLocaleString(getLang()) : null);

/**
 * The details of a video download, like a file download's: progress, speed (live chart, average, peak), time,
 * size, the player, what was asked for (quality, file type, sound only, subtitles), the link, the folder and
 * the saved files, notes and errors, with every action the list offers.
 */
export function MediaDetailView({ id, onBack }: { id: number; onBack: () => void }) {
  const t = useT();
  const job = useStore((s) => s.media.find((j) => j.id === id));
  const [now, setNow] = useState(() => Date.now());
  const [speeds, setSpeeds] = useState<number[]>([]);
  const [ready, setReady] = useState(false);
  const speed = useRef(0);
  speed.current = job?.speedBps ?? 0;
  const seen = useRef(false);
  if (job) seen.current = true;

  // A live chart and clock: one reading a second (the job's own events carry the speed).
  useEffect(() => {
    const tick = setInterval(() => {
      setNow(Date.now());
      setSpeeds((s) => [...s, speed.current].slice(-90));
    }, 1000);
    return () => clearInterval(tick);
  }, []);
  useEffect(() => void loadPreviewBase().then(() => setReady(true)), []);
  // Removed from the list while open: back to the list.
  useEffect(() => {
    if (!job && seen.current) onBack();
  }, [job, onBack]);

  if (!job) {
    return (
      <div className="space-y-4">
        <button className="btn btn-ghost -ms-2" onClick={onBack}>
          <Icon name="back" className="flip-rtl" /> {t("det.back")}
        </button>
        <p className="text-dim">{t("md.gone")}</p>
      </div>
    );
  }

  const done = job.status === "completed";
  const running = job.status === "preparing" || job.status === "downloading" || job.status === "combining";
  const size = sizeOf(job);
  const peak = Math.max(0, ...speeds);
  const avg = averageBps(job, now);
  const took = elapsedMs(job, now);
  const paths = savedPaths(job);
  const remaining = size?.estimate ? Math.max(0, size.bytes - job.bytes) : null;

  return (
    <div className="space-y-4" data-testid="media-detail-view">
      <div className="flex flex-wrap items-center gap-3">
        <button className="btn btn-ghost btn-icon -ms-2" onClick={onBack} aria-label={t("det.backTo")} data-testid="media-detail-back">
          <Icon name="back" size={18} className="flip-rtl" />
        </button>
        <div className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-accent-soft text-accent">
          <Icon name={job.audioOnly ? "music" : "film"} size={19} />
        </div>
        <div className="min-w-0 flex-1">
          <h1 className="truncate text-lg font-semibold tracking-tight" data-testid="media-detail-title">
            {job.title}
          </h1>
          <p className="truncate text-xs text-dim">{job.url ?? job.host}</p>
        </div>
        <span className="chip" data-testid="media-detail-status">
          {t(mediaLabel(job.status))}
        </span>
        <MediaControls job={job} size="md" />
      </div>

      {job.error && (
        <section className="card flex items-start gap-3 border-bad/40 p-4" role="alert" data-testid="media-detail-error">
          <Icon name="alert" size={18} className="mt-0.5 shrink-0 text-bad" />
          <p className="selectable min-w-0 flex-1 break-words text-[13px] font-medium">{job.error}</p>
        </section>
      )}
      {job.notes.length > 0 && (
        <section className="card space-y-1 border-warn/40 p-3 text-xs text-warn" data-testid="media-detail-notes">
          {job.notes.map((n, i) => (
            <p key={i}>{localNote(n)}</p>
          ))}
        </section>
      )}

      <Card title={t("det.overview")}>
        <div className="mb-3 flex items-end justify-between">
          <span className="num text-3xl font-semibold leading-none tracking-tight" data-testid="media-detail-percent">
            {done ? "100%" : job.percent !== null ? `${job.percent.toFixed(1)}%` : "—"}
          </span>
          <span className="num text-xs text-dim">
            {formatBytes(job.bytes)}
            {size ? ` / ${size.estimate ? "~" : ""}${formatBytes(size.bytes)}` : ""}
          </span>
        </div>
        <ProgressBar value={done ? 1 : (job.percent ?? 0) / 100} label={t("det.overall")} tone={mediaTone(job.status)} live={running} className="h-2" />
        <div className="mt-4 grid grid-cols-2 gap-2.5 sm:grid-cols-3 lg:grid-cols-6">
          <Stat
            label={t("det.progress")}
            value={job.segmentsTotal > 0 ? t("md.pieces", { done: job.segmentsDone, total: job.segmentsTotal }) : job.percent !== null || done ? `${done ? 100 : Math.floor(job.percent ?? 0)}%` : "—"}
          />
          <Stat label={t("det.speed")} value={formatSpeed(running ? job.speedBps : 0)} testId="media-detail-speed" />
          <Stat label={t("det.eta")} value={running ? formatEta(job.etaSecs) : "—"} />
          <Stat label={t("det.downloaded")} value={formatBytes(job.bytes)} testId="media-detail-bytes" />
          <Stat label={t("det.remaining")} value={done ? formatBytes(0) : remaining !== null ? `~${formatBytes(remaining)}` : "—"} />
          <Stat label={running ? t("md.runningFor") : t("md.took")} value={took >= 1000 ? formatDuration(Math.round(took / 1000)) : "—"} testId="media-detail-time" />
        </div>
      </Card>

      <div className="grid gap-4 lg:grid-cols-2">
        <Card title={t("det.network")}>
          <Sparkline values={speeds} label={t("det.throughput")} className="h-16" />
          <dl className="mt-3 divide-y divide-line">
            <Meta k={t("det.current")} v={formatSpeed(running ? job.speedBps : 0)} />
            <Meta k={t("det.average")} v={avg > 0 ? formatSpeed(avg) : "—"} />
            <Meta k={t("det.peak")} v={peak > 0 ? formatSpeed(peak) : "—"} />
            <Meta k={t("det.retries")} v={String(job.retries ?? 0)} />
          </dl>
        </Card>

        <Card title={t("md.player")} testId="media-detail-player">
          {job.playable && ready ? <MediaPlayer job={job} /> : <p className="text-dim">{t("md.notYet")}</p>}
        </Card>
      </div>

      <Card
        title={t("det.segments")}
        testId="media-detail-pieces"
        right={job.segmentsTotal > 0 ? <span className="num text-xs text-dim">{t("det.nComplete", { done: job.segmentsDone, total: job.segmentsTotal })}</span> : undefined}
      >
        {job.segmentsTotal > 0 ? (
          <PieceMap done={done ? job.segmentsTotal : job.segmentsDone} total={job.segmentsTotal} />
        ) : (
          <p className="text-dim">{job.site ? t("md.noPiecesSite") : t("det.noChunks")}</p>
        )}
      </Card>

      <Card title={t("det.metadata")} testId="media-detail-meta">
        <dl className="divide-y divide-line">
          <Meta k={t("det.url")} v={job.url ?? null} copy />
          <Meta k={t("md.site")} v={job.host} />
          <Meta k={t("det.kind")} v={t(typeKey(job))} />
          <Meta k={t("md.quality")} v={qualityText(job.quality) ?? t("md.best")} />
          <Meta k={t("md.format")} v={job.format ? job.format.toUpperCase() : t("md.formatAuto")} />
          <Meta k={t("md.subtitles")} v={job.subtitles ? String(job.subtitles) : t("common.none")} />
          <Meta k={t("det.size")} v={done && size ? t("det.bytes", { size: formatBytes(size.bytes), n: size.bytes.toLocaleString(getLang()) }) : null} />
          <Meta k={t("md.started")} v={when(job.startedAtMs)} />
          <Meta k={done ? t("md.ended") : t("md.stopped")} v={running ? null : when(job.endedAtMs)} />
          <Meta k={t("md.folder")} v={job.folder} copy />
          {paths.length > 0 ? paths.map((p, i) => <Meta key={p} k={i === 0 ? t("det.savedTo") : ""} v={p} copy />) : <Meta k={t("det.savedTo")} v={null} />}
        </dl>
      </Card>
    </div>
  );
}
