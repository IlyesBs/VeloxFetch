import { useEffect, useState } from "react";
import { api, normalizeError } from "../api";
import { act } from "../hooks";
import { formatBytes } from "../format";
import { useT } from "../i18n";
import type { DownloadStatus, PreviewInfo } from "../types";
import { Icon } from "../ui/icons";
import { loadPreviewBase, previewSrc } from "../preview";

/** The address the player reads from, by download id (never a path); see `preview.ts`. */
export function previewUrl(id: number): string {
  return previewSrc(String(id));
}

const PLAYABLE: DownloadStatus[] = ["downloading", "queued", "paused", "completed"];

/**
 * Watch a video (or listen to audio) while it downloads. Preview mode makes the engine fetch the start and the
 * end of the file first (players need both), then everything in order; the player reads only what is already on
 * disk and waits for the rest.
 */
export function PreviewCard({ id, status }: { id: number; status: DownloadStatus }) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const [info, setInfo] = useState<PreviewInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    let alive = true;
    const load = () =>
      api.getPreviewInfo(id).then(
        (i) => alive && (setInfo(i), setError(null)),
        (e) => alive && setError(normalizeError(e).message),
      );
    void load();
    const timer = setInterval(load, 1000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [open, id]);

  const start = async () => {
    await loadPreviewBase();
    const i = await act(() => api.setPreviewMode(id, true));
    if (i) {
      setInfo(i);
      setOpen(true);
    }
  };

  const close = () => {
    setOpen(false);
    void api.setPreviewMode(id, false).catch(() => {});
  };

  if (!PLAYABLE.includes(status)) return null;
  const pct = info && info.total > 0 ? Math.round((info.readyFromStart / info.total) * 100) : 0;
  return (
    <section className="card p-4" data-testid="preview-card">
      <div className="flex items-center justify-between gap-3">
        <div>
          <h2 className="text-[13px] font-semibold">{t("prev.title")}</h2>
          <p className="text-xs text-dim">{status === "completed" ? t("prev.done") : t("prev.while")}</p>
        </div>
        {open ? (
          <button className="btn" data-testid="preview-close" onClick={close}>
            {t("common.close")}
          </button>
        ) : (
          <button className="btn btn-primary" data-testid="preview-open" onClick={() => void start()}>
            <Icon name="play" size={14} /> {status === "completed" ? t("prev.play") : t("prev.watch")}
          </button>
        )}
      </div>
      {open && (
        <div className="mt-3 space-y-2">
          {/* No crossOrigin, no download / remote playback: the stream stays inside this window. */}
          <video
            data-testid="preview-video"
            className="aspect-video w-full rounded-xl bg-black"
            src={previewUrl(id)}
            controls
            autoPlay
            preload="auto"
            controlsList="nodownload noremoteplayback"
            disablePictureInPicture
          />
          <p className="text-xs text-dim" data-testid="preview-ready">
            {error
              ? error
              : info && !info.complete
                ? t("prev.ready", { got: formatBytes(info.readyFromStart), pct })
                : t("prev.whole")}
          </p>
        </div>
      )}
    </section>
  );
}
