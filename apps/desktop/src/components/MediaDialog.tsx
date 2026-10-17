import { useEffect, useState } from "react";
import { api, normalizeError } from "../api";
import { act, store } from "../hooks";
import { t as tr, useT, type Key } from "../i18n";
import type { MediaAnalysis, MediaRequest, Playlist } from "../types";
import { Icon } from "../ui/icons";

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
  /** A playlist: the videos chosen (positions in it). */
  const [picks, setPicks] = useState<number[]>([]);
  /** The playlist was read from a video of it ("Download the whole playlist"): "Only this video" goes back. */
  const [whole, setWhole] = useState(false);

  const look = async (u = url, wholePlaylist = false) => {
    if (!u.trim()) return;
    setWhole(wholePlaylist);
    setLooking(true);
    setSlow(false);
    setError(null);
    setAnalysis(null);
    const hint = setTimeout(() => setSlow(true), 3000);
    try {
      const a = await api.analyzeMedia(u.trim(), pending, wholePlaylist);
      setAnalysis(a);
      setStream(a.streams[0]?.id ?? "");
      setAudio("");
      setSubs([]);
      setName(a.title);
      setPicks(a.playlist ? a.playlist.entries.filter((e) => e.available).map((e) => e.index) : []);
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

  const playlist = analysis?.playlist ?? null;
  const startPlaylist = async () => {
    if (!analysis?.playlistKey || !playlist) return;
    setBusy(true);
    const jobs = await act(() =>
      api.startPlaylist({
        key: analysis.playlistKey!,
        items: picks,
        ...(stream && { quality: stream }),
        format: fileType,
        ...(subdir.trim() && { subdir: subdir.trim() }),
        ...(pending !== undefined && { pending }),
      }),
    );
    setBusy(false);
    if (!jobs) return;
    store.toast("info", t("mdl.playlistStarted", { n: jobs.length, title: playlist.title }));
    onClose();
  };

  const start = async () => {
    if (!analysis) return;
    if (playlist) return startPlaylist();
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
          {analysis?.playlist ? t("mdl.titlePlaylist") : pending !== undefined ? t("mdl.titlePending") : t("mdl.title")}
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
            {playlist ? (
              <PlaylistHeader playlist={playlist} onlyThis={whole ? () => void look(url, false) : undefined} />
            ) : (
              <p className="text-xs text-dim">
                {analysis.kind === "site" ? t("mdl.page") : analysis.kind === "hls" ? t("mdl.hls") : t("mdl.dash")}
                {duration(analysis.durationSecs) ? ` · ${duration(analysis.durationSecs)}` : ""}
              </p>
            )}

            {analysis.partOfPlaylist && !playlist && (
              <div className="flex flex-wrap items-center gap-2 rounded-xl border border-accent/40 bg-accent-soft p-3" data-testid="media-part-of-playlist">
                <Icon name="queue" size={16} className="shrink-0 text-accent" />
                <span className="min-w-0 flex-1 text-[13px]">{t("mdl.partOfPlaylist")}</span>
                <button type="button" className="btn btn-primary h-8" data-testid="media-whole-playlist" disabled={looking} onClick={() => void look(url, true)}>
                  {t("mdl.wholePlaylist")}
                </button>
              </div>
            )}

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

            {playlist && <PlaylistPicker playlist={playlist} picks={picks} setPicks={setPicks} />}

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

            {playlist ? (
              <label className="block space-y-1">
                <span className="label">{t("mdl.playlistFolder")}</span>
                <input data-testid="media-subdir" className="input h-9" value={subdir} onChange={(e) => setSubdir(e.target.value)} placeholder={playlist.title} />
                <span className="block text-xs text-dim">{t("mdl.playlistFolderHint")}</span>
              </label>
            ) : (
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
            )}

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
          <button className="btn btn-primary" data-testid="media-start" disabled={!analysis || busy || !stream || (!!playlist && picks.length === 0)} onClick={() => void start()}>
            {playlist ? t("mdl.downloadVideos", { n: picks.length }) : t("mdl.download")}
          </button>
        </div>
      </div>
    </div>
  );
}

/** The playlist's name and size, highlighted: what the window is about to download. */
function PlaylistHeader({ playlist, onlyThis }: { playlist: Playlist; onlyThis?: () => void }) {
  const t = useT();
  const shown = playlist.entries.length;
  return (
    <div className="flex items-start gap-3 rounded-xl border border-accent/40 bg-accent-soft p-3" data-testid="media-playlist-header">
      <div className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-accent text-white">
        <Icon name="queue" size={17} />
      </div>
      <div className="min-w-0 flex-1">
        <p className="text-[11px] font-semibold uppercase tracking-wide text-accent">{t("mdl.playlist")}</p>
        <p className="truncate text-[14px] font-semibold" title={playlist.title} data-testid="media-playlist-title">
          {playlist.title}
        </p>
        <p className="text-xs text-dim" data-testid="media-playlist-count">
          {t("mdl.playlistCount", { n: shown })}
          {playlist.truncated ? ` · ${t("mdl.playlistMore", { total: playlist.total ?? `${shown}+`, n: shown })}` : ""}
        </p>
      </div>
      {onlyThis && (
        <button type="button" className="btn h-8 shrink-0 text-xs" data-testid="media-only-this" onClick={onlyThis}>
          {t("mdl.onlyThis")}
        </button>
      )}
    </div>
  );
}

/** The playlist's videos with a box each (all chosen at first; private, deleted and live ones cannot be). */
function PlaylistPicker({ playlist, picks, setPicks }: { playlist: Playlist; picks: number[]; setPicks: (p: number[]) => void }) {
  const t = useT();
  const usable = playlist.entries.filter((e) => e.available).map((e) => e.index);
  const all = usable.length > 0 && picks.length === usable.length;
  const toggle = (i: number, on: boolean) => setPicks(on ? [...picks, i].sort((a, b) => a - b) : picks.filter((x) => x !== i));
  return (
    // min-w-0: a fieldset is as wide as its longest title by default, which pushed the dialog sideways.
    <fieldset className="min-w-0 space-y-1.5">
      <div className="flex items-center gap-2">
        <label className="flex cursor-pointer items-center gap-2 text-[13px] font-medium">
          <input type="checkbox" data-testid="media-pick-all" checked={all} onChange={(e) => setPicks(e.target.checked ? usable : [])} />
          {t("mdl.selectAll")}
        </label>
        <span className="num ms-auto text-xs text-dim" data-testid="media-picked">
          {t("mdl.chosen", { n: picks.length, total: usable.length })}
        </span>
      </div>
      <div className="max-h-56 overflow-y-auto rounded-xl border border-line" data-testid="media-playlist-items">
        {playlist.entries.map((e) => (
          <label
            key={e.index}
            className={`flex items-center gap-2.5 border-b border-line px-3 py-1.5 text-[13px] last:border-b-0 ${e.available ? "cursor-pointer hover:bg-accent-soft/50" : "opacity-55"}`}
            data-testid={`media-item-${e.index}`}
          >
            <input type="checkbox" disabled={!e.available} checked={picks.includes(e.index)} onChange={(ev) => toggle(e.index, ev.target.checked)} />
            <span className="num w-8 shrink-0 text-end text-xs text-dim">{e.index}</span>
            <span className="min-w-0 flex-1 truncate" title={e.title}>
              {e.title}
            </span>
            <span className="num shrink-0 text-xs text-dim">{e.available ? duration(e.durationSecs) : t("mdl.unavailable")}</span>
          </label>
        ))}
      </div>
    </fieldset>
  );
}
