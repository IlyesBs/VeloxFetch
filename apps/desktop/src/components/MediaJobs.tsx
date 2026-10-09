import { memo, useState } from "react";
import { loadPreviewBase, previewSrc } from "../preview";
import { actions } from "../actions";
import { api } from "../api";
import { formatBytes, formatEta, formatSpeed } from "../format";
import { act, useStore } from "../hooks";
import type { MediaJob, MediaStatus } from "../types";
import { Icon } from "../ui/icons";
import { t as tr, useT, type Key } from "../i18n";
import { ProgressBar, Section } from "../ui/primitives";
import { DeleteConfirm } from "./DeleteConfirm";

const LABEL: Record<MediaStatus, Key> = {
  preparing: "mj.preparing",
  downloading: "status.downloading",
  combining: "mj.finishing",
  completed: "status.completed",
  failed: "status.failed",
  cancelled: "status.cancelled",
  paused: "status.paused",
};

/** The engine's fixed notes in the app's language (others are shown as they come). */
export function localNote(note: string): string {
  const fixed: Record<string, Key> = {
    "Downloading required dependencies for HD (only the first time)…": "note.depsHd",
    "Downloading required dependencies to join video and sound (only the first time)…": "note.depsJoin",
    "Updating required dependencies (the site changed)…": "note.depsUpdate",
    "The required dependencies are not ready, so the video was saved as the site sends it (no HD join, no file type change). Download them in Settings > Required dependencies.":
      "note.noDepsSite",
    "The required dependencies are not ready, so the sound is a separate file. Download them in Settings > Required dependencies to have VeloxFetch combine them.":
      "note.noDepsJoin",
  };
  if (fixed[note]) return tr(fixed[note]);
  const retry = /^It stopped \(.*\)\. Trying again \((\d+) of (\d+)\)…$/.exec(note);
  if (retry) return tr("note.retrying", { n: retry[1]!, total: retry[2]! });
  return note;
}

const TONE: Record<MediaStatus, "accent" | "ok" | "bad" | "warn"> = {
  preparing: "accent",
  downloading: "accent",
  combining: "accent",
  completed: "ok",
  failed: "bad",
  cancelled: "warn",
  paused: "warn",
};

/** The player's address, by job id (never a path); see `preview.ts`. */
export function mediaPreviewUrl(id: number): string {
  return previewSrc(`m${id}`);
}

const Row = memo(function Row({ job }: { job: MediaJob }) {
  const t = useT();
  const [watching, setWatching] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const active = job.status === "preparing" || job.status === "downloading" || job.status === "combining";
  const done = job.status === "completed";
  const target = { kind: "media" as const, id: job.id };
  return (
    <article className="card rise p-4" data-testid={`media-${job.id}`} data-status={job.status}>
      {confirm && (
        <DeleteConfirm
          name={job.files[0] ?? job.title}
          onDelete={() => void act(() => api.clearMediaJob(job.id, true), t("done.deleted", { name: job.files[0] ?? job.title }))}
          onRemove={() => void act(() => api.clearMediaJob(job.id))}
          onClose={() => setConfirm(false)}
        />
      )}
      <div className="flex items-start gap-3">
        <div className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-accent-soft text-accent">
          <Icon name="film" size={19} />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <p className="truncate text-[14px] font-semibold" title={job.title}>
              {job.title}
            </p>
            <span className="chip" data-testid="media-status">
              {t(LABEL[job.status])}
            </span>
          </div>
          <p className="truncate text-xs text-dim" title={job.folder}>
            {job.host} · {job.site ? t("mj.video") : t("mj.stream")}
          </p>
        </div>
        <span className="num text-lg font-semibold leading-none">{job.percent === null ? "—" : `${Math.floor(job.percent)}%`}</span>
      </div>
      <ProgressBar value={done ? 1 : (job.percent ?? 0) / 100} tone={TONE[job.status]} label={t("mj.progress", { title: job.title })} className="mt-3.5" />
      <div className="mt-2.5 flex flex-wrap items-center gap-x-3 gap-y-2">
        <p className="num flex-1 text-xs text-dim" data-testid="media-numbers">
          {formatBytes(job.bytes)}
          {active && job.speedBps > 0 ? ` · ${formatSpeed(job.speedBps)}` : ""}
          {active && job.etaSecs !== null ? ` · ${formatEta(job.etaSecs)} left` : ""}
          {job.segmentsTotal > 0 ? ` · ${job.segmentsDone} of ${job.segmentsTotal} pieces` : ""}
        </p>
        <span className="flex flex-wrap items-center gap-1.5">
          {(job.playable || watching) && (
            <button className="btn h-8 px-2.5 text-xs" data-testid="media-play" onClick={() => void loadPreviewBase().then(() => setWatching(!watching))} aria-pressed={watching}>
              <Icon name="play" size={13} /> {watching ? t("mj.hidePlayer") : done ? t("prev.play") : t("mj.watchNow")}
            </button>
          )}
          {active && (
            <button className="btn h-8 px-2.5 text-xs" data-testid="media-pause" onClick={() => void act(() => api.pauseMediaJob(job.id))}>
              <Icon name="pause" size={13} /> {t("mj.pause")}
            </button>
          )}
          {job.resumable && (
            <button className="btn btn-primary h-8 px-2.5 text-xs" data-testid="media-resume" onClick={() => void act(() => api.resumeMediaJob(job.id))}>
              <Icon name={job.status === "paused" ? "play" : "retry"} size={13} /> {job.status === "paused" ? t("mj.resume") : t("mj.retry")}
            </button>
          )}
          {done && (
            <button className="btn h-8 px-2.5 text-xs" data-testid="media-open" onClick={() => void actions.openFile(target)}>
              <Icon name="external" size={13} /> {t("mj.open")}
            </button>
          )}
          {job.status !== "cancelled" && (
            <button className="btn h-8 px-2.5 text-xs" data-testid="media-show" onClick={() => void actions.showInFolder(target)}>
              <Icon name="folder" size={13} /> {t("mj.showFolder")}
            </button>
          )}
          {active || job.status === "paused" || (job.status === "failed" && job.resumable) ? (
            <button className="btn h-8 px-2.5 text-xs" data-testid="media-cancel-job" onClick={() => void act(() => api.cancelMediaJob(job.id))}>
              <Icon name="x" size={13} /> {t("common.cancel")}
            </button>
          ) : (
            <>
              {done && (
                <button className="btn h-8 px-2.5 text-xs text-bad" data-testid="media-delete-job" aria-label={t("media.delete")} title={t("media.delete")} onClick={() => setConfirm(true)}>
                  <Icon name="trash" size={14} />
                </button>
              )}
              <button className="btn h-8 px-2.5 text-xs" data-testid="media-clear-job" aria-label={t("mj.removeFromList", { title: job.title })} title={t("card.remove")} onClick={() => void act(() => api.clearMediaJob(job.id))}>
                <Icon name="x" size={14} />
              </button>
            </>
          )}
        </span>
      </div>
      {watching && (
        <div className="mt-3 space-y-1.5">
          {/* No crossOrigin, no download / remote playback: the stream stays inside this window. */}
          <video
            data-testid="media-player"
            className="aspect-video w-full rounded-xl bg-black"
            src={mediaPreviewUrl(job.id)}
            controls
            autoPlay
            preload="auto"
            controlsList="nodownload noremoteplayback"
            disablePictureInPicture
          />
          {!done && (
            <p className="text-xs text-dim">
              {job.site ? t("mj.playingPartSite") : t("mj.playingPart")}
            </p>
          )}
        </div>
      )}
      {done && job.files.length > 0 && (
        <p className="mt-2 text-xs" data-testid="media-files">
          {t("mj.saved")} <span className="selectable font-medium">{job.files.join(", ")}</span>
        </p>
      )}
      {job.notes.map((n, i) => (
        <p key={i} className="mt-2 text-xs text-warn" data-testid="media-note">
          {localNote(n)}
        </p>
      ))}
      {job.error && (
        <p className="mt-2 flex items-start gap-1.5 text-xs text-bad" role="alert">
          <Icon name="alert" size={14} className="mt-px shrink-0" />
          {job.error}
        </p>
      )}
    </article>
  );
});

const RUNNING: MediaStatus[] = ["preparing", "downloading", "combining", "paused"];

/**
 * Video downloads (YouTube and similar sites, HLS / DASH streams). A separate list: they are not rows of the
 * normal download list. `only`: just the running ones (Active page) or just the finished ones (Completed page).
 */
export function MediaJobs({ only }: { only?: "active" | "done" }) {
  const t = useT();
  const all = useStore((s) => s.media);
  const media = only === "active" ? all.filter((j) => RUNNING.includes(j.status)) : only === "done" ? all.filter((j) => j.status === "completed") : all;
  if (media.length === 0) return null;
  return (
    <Section title={t("sec.videos")} hint={`${media.length}`}>
      <div className="space-y-3" data-testid={only ? `media-jobs-${only}` : "media-jobs"}>
        {[...media].reverse().map((j) => (
          <Row key={j.id} job={j} />
        ))}
      </div>
    </Section>
  );
}
