import { useEffect, useState } from "react";
import { api, normalizeError } from "../api";
import { act, store } from "../hooks";
import { t as tr, useT, type Key } from "../i18n";
import type { MediaAnalysis, MediaRequest } from "../types";

function duration(secs: number | null): string {
  if (!secs || secs <= 0) return "";
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = Math.floor(secs % 60);
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}` : `${m}:${String(s).padStart(2, "0")}`;
}

// Labels are keys (translated) or plain format names.
const VIDEO_TYPES: { id: string; label: Key | string }[] = [
  { id: "mp4", label: "mdl.mp4" },
  { id: "mkv", label: "mdl.mkv" },
  { id: "webm", label: "WebM" },
];
const AUDIO_TYPES: { id: string; label: Key | string }[] = [
  { id: "mp3", label: "mdl.mp3" },
  { id: "m4a", label: "M4A (AAC)" },
  { id: "opus", label: "Opus" },
];
const typeLabel = (l: string) => (l.startsWith("mdl.") ? tr(l as Key) : l);

/** The engine's quality and track labels ("Audio only · about 4 MB", "720p · 2.5 Mbit/s") in the app's language. */
export function localLabel(label: string): string {
  return label
    .split(" · ")
    .map((part) => {
      if (part === "Best available") return tr("lbl.best");
      if (part === "Audio only") return tr("lbl.audioOnly");
      if (part === "Video") return tr("lbl.video");
      if (part === "Audio") return tr("lbl.audio");
      if (part === "Subtitles") return tr("lbl.subtitles");
      if (part.startsWith("about ")) return `${tr("lbl.about")} ${part.slice(6)}`;
      if (part.endsWith("px wide")) return `${part.slice(0, -5)} ${tr("lbl.wide")}`;
      return part;
    })
    .join(" · ");
}
/** The last types chosen, kept while the app runs (nothing is stored on disk by the web view). */
const lastType = { video: "mp4", audio: "mp3" };

/**
 * Download a video stream (HLS `.m3u8` / DASH `.mpd`): look at the link, choose a quality, an audio
 * track and subtitles, then start. The analysis writes nothing; the choices go back to Rust as ids
 * (never as addresses), and Rust re-reads the stream and refuses anything unknown.
 */
export function MediaDialog({ initialUrl, pending, onClose }: { initialUrl: string; pending?: number; onClose: () => void }) {
  const t = useT();
  const [url, setUrl] = useState(initialUrl);
  const [analysis, setAnalysis] = useState<MediaAnalysis | null>(null);
  const [looking, setLooking] = useState(false);
  /** Looking takes a while: the first time, the tools for video sites are being fetched. */
  const [slow, setSlow] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [stream, setStream] = useState("");
  const [audio, setAudio] = useState("");
  const [subs, setSubs] = useState<string[]>([]);
  const [combine, setCombine] = useState(true);
  const [videoType, setVideoType] = useState(lastType.video);
  const [audioType, setAudioType] = useState(lastType.audio);
  const [subdir, setSubdir] = useState("");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);

  const look = async (u = url) => {
    if (!u.trim()) return;
    setLooking(true);
    setSlow(false);
    setError(null);
    setAnalysis(null);
    const hint = setTimeout(() => setSlow(true), 3000);
    try {
      const a = await api.analyzeMedia(u.trim(), pending);
      setAnalysis(a);
      setStream(a.streams[0]?.id ?? "");
      setAudio("");
      setSubs([]);
      setName(a.title);
    } catch (e) {
      setError(normalizeError(e).message);
    } finally {
      clearTimeout(hint);
      setLooking(false);
      setSlow(false);
    }
  };

  useEffect(() => {
    if (initialUrl.trim()) void look(initialUrl);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const esc = (e: KeyboardEvent) => e.key === "Escape" && !busy && onClose();
    document.addEventListener("keydown", esc);
    return () => document.removeEventListener("keydown", esc);
  }, [busy, onClose]);

  const soundOnly = stream === "audio";
  const site = analysis?.kind === "site";
  // Streams are video: a type other than the one they come in needs ffmpeg (joining).
  const typeAvailable = !!analysis && (site || (analysis.combineAvailable && combine));
  const fileType = soundOnly ? audioType : site ? videoType : VIDEO_TYPES.some((v) => v.id === videoType) ? videoType : "mp4";
  const chooseType = (id: string) => {
    if (soundOnly) {
      setAudioType(id);
      lastType.audio = id;
    } else {
      setVideoType(id);
      lastType.video = id;
    }
  };

  const start = async () => {
    if (!analysis) return;
    const req: MediaRequest = {
      url: url.trim(),
      stream,
      combine,
      ...(typeAvailable && { format: fileType }),
      ...(audio && { audio }),
      ...(subs.length > 0 && { subtitles: subs }),
      ...(subdir.trim() && { subdir: subdir.trim() }),
      ...(name.trim() && { filename: name.trim() }),
      ...(pending !== undefined && { pending }),
    };
    setBusy(true);
    const job = await act(() => api.startMedia(req));
    setBusy(false);
    if (!job) return;
    store.toast("info", t("mdl.started", { title: job.title }));
    onClose();
  };

  // The audio tracks that belong with the chosen quality (HLS groups; DASH has one group).
  const chosen = analysis?.streams.find((s) => s.id === stream);
  const tracks = analysis ? analysis.audio.filter((a) => !chosen?.group || !a.group || a.group === chosen.group) : [];
  const separateAudio = tracks.length > 0 && audio !== "none";

  return (
    <div className="fixed inset-0 z-40 grid place-items-center bg-black/45 p-4" data-testid="media-overlay">
      <div className="card rise flex max-h-[92vh] w-full max-w-xl flex-col p-5 shadow-xl" role="dialog" aria-modal="true" aria-labelledby="media-title" data-testid="media-dialog">
        <h2 id="media-title" className="text-[15px] font-semibold">
          {pending !== undefined ? t("mdl.titlePending") : t("mdl.title")}
        </h2>
        <p className="mt-1 text-xs text-dim">
          {t("mdl.intro")}
        </p>

        <form
          className="mt-3 flex gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            void look();
          }}
        >
          <input
            data-testid="media-url"
            className="input h-9 flex-1"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            placeholder={t("mdl.urlEg")}
            spellCheck={false}
            autoFocus={!initialUrl}
            readOnly={pending !== undefined}
            title={pending !== undefined ? t("mdl.fromBrowser") : undefined}
          />
          <button type="submit" className="btn h-9" data-testid="media-look" disabled={looking || !url.trim()}>
            {looking ? t("mdl.looking") : t("mdl.look")}
          </button>
        </form>

        {looking && slow && (
          <p className="mt-3 text-xs text-dim" role="status" data-testid="media-slow">
            {t("mdl.slow")}
          </p>
        )}

        {error && (
          <p className="mt-3 rounded-xl border border-line p-3 text-xs text-bad" role="alert" data-testid="media-error">
            {error}
          </p>
        )}

        {analysis && (
          <div className="mt-3 min-h-0 flex-1 space-y-3 overflow-y-auto pe-1" data-testid="media-options">
            <p className="text-xs text-dim">
              {analysis.kind === "site" ? t("mdl.page") : analysis.kind === "hls" ? t("mdl.hls") : t("mdl.dash")}
              {duration(analysis.durationSecs) ? ` · ${duration(analysis.durationSecs)}` : ""}
            </p>

            <label className="block space-y-1">
              <span className="label">{t("mdl.quality")}</span>
              <select className="input h-9" data-testid="media-quality" value={stream} onChange={(e) => setStream(e.target.value)}>
                {analysis.streams.map((s) => (
                  <option key={s.id} value={s.id}>
                    {localLabel(s.label)}
                    {s.codecs ? ` · ${s.codecs}` : ""}
                  </option>
                ))}
              </select>
            </label>

            {typeAvailable && (
              <label className="block space-y-1">
                <span className="label">{t("mdl.type")}</span>
                <select className="input h-9" data-testid="media-format" value={fileType} onChange={(e) => chooseType(e.target.value)}>
                  {soundOnly ? (
                    AUDIO_TYPES.map((x) => (
                      <option key={x.id} value={x.id}>
                        {typeLabel(x.label)}
                      </option>
                    ))
                  ) : (
                    <>
                      <optgroup label={t("mdl.video")}>
                        {VIDEO_TYPES.map((x) => (
                          <option key={x.id} value={x.id}>
                            {typeLabel(x.label)}
                          </option>
                        ))}
                      </optgroup>
                      {site && (
                        <optgroup label={t("mdl.soundOnly")}>
                          {AUDIO_TYPES.map((x) => (
                            <option key={x.id} value={x.id}>
                              {typeLabel(x.label)}
                            </option>
                          ))}
                        </optgroup>
                      )}
                    </>
                  )}
                </select>
              </label>
            )}

            {tracks.length > 0 && (
              <label className="block space-y-1">
                <span className="label">{t("mdl.audio")}</span>
                <select className="input h-9" data-testid="media-audio" value={audio} onChange={(e) => setAudio(e.target.value)}>
                  <option value="">
                    {t("mdl.default")}
                    {tracks.find((x) => x.default) ? ` (${localLabel(tracks.find((x) => x.default)!.label)})` : ""}
                  </option>
                  {tracks.map((x) => (
                    <option key={x.id} value={x.id}>
                      {localLabel(x.label)}
                    </option>
                  ))}
                  <option value="none">{t("mdl.noAudio")}</option>
                </select>
              </label>
            )}

            {analysis.subtitles.length > 0 && (
              <fieldset className="space-y-1">
                <legend className="label">{t("mdl.subs")}</legend>
                {analysis.subtitles.map((sub) => (
                  <label key={sub.id} className="flex cursor-pointer items-center gap-2 text-[13px]">
                    <input
                      type="checkbox"
                      data-testid={`media-sub-${sub.id}`}
                      checked={subs.includes(sub.id)}
                      onChange={(e) => setSubs(e.target.checked ? [...subs, sub.id] : subs.filter((x) => x !== sub.id))}
                    />
                    {localLabel(sub.label)}
                  </label>
                ))}
              </fieldset>
            )}

            <div className="grid grid-cols-2 gap-3">
              <label className="space-y-1">
                <span className="label">{t("addd.filename")}</span>
                <input data-testid="media-name" className="input h-9" value={name} onChange={(e) => setName(e.target.value)} />
              </label>
              <label className="space-y-1">
                <span className="label">{t("mdl.subdir")}</span>
                <input data-testid="media-subdir" className="input h-9" value={subdir} onChange={(e) => setSubdir(e.target.value)} placeholder={t("mdl.subdirEg")} />
              </label>
            </div>

            {separateAudio && (
              <label className="flex cursor-pointer items-start gap-2 text-[13px]">
                <input type="checkbox" data-testid="media-combine" className="mt-0.5" checked={combine && analysis.combineAvailable} disabled={!analysis.combineAvailable} onChange={(e) => setCombine(e.target.checked)} />
                <span>
                  {t("mdl.combine")}
                  {!analysis.combineAvailable && (
                    <span className="block text-xs text-dim" data-testid="media-no-ffmpeg">
                      {t("mdl.noJoin")}
                    </span>
                  )}
                </span>
              </label>
            )}
          </div>
        )}

        <div className="mt-4 flex items-center justify-end gap-2">
          <button className="btn" data-testid="media-cancel" disabled={busy} onClick={onClose}>
            {t("common.cancel")}
          </button>
          <button className="btn btn-primary" data-testid="media-start" disabled={!analysis || busy || !stream} onClick={() => void start()}>
            {t("mdl.download")}
          </button>
        </div>
      </div>
    </div>
  );
}
