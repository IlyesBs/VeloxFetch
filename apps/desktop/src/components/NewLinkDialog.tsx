import { useEffect, useRef, useState, type FormEvent } from "react";
import { createPortal } from "react-dom";
import { api, normalizeError } from "../api";
import { formatBytes } from "../format";
import { act, useDownload } from "../hooks";
import { useT } from "../i18n";
import { NEW_LINK, outcomeView } from "../newLink";
import type { NewLinkOutcome } from "../types";
import { Icon } from "../ui/icons";

const TONE = { ok: "text-ok", warn: "text-warn", bad: "text-bad" } as const;

/** The result of a check: what happened, the evidence, and that the saved bytes are kept. */
export function NewLinkResult({ o }: { o: NewLinkOutcome }) {
  const t = useT();
  const v = outcomeView(o);
  return (
    <div className="space-y-1 rounded-lg border border-line px-3 py-2 text-xs" role="status" data-testid="newlink-result" data-reason={o.reason} data-applied={o.applied}>
      <p className={`font-semibold ${TONE[v.tone]}`}>{t(v.title)}</p>
      <p>{t(v.detail)}</p>
      {o.httpStatus !== null && <p className="text-dim">{t("nl.status", { code: o.httpStatus })}</p>}
      {!o.applied && o.keptBytes > 0 && <p className="text-dim">{t("nl.kept", { size: formatBytes(o.keptBytes) })}</p>}
    </div>
  );
}

/**
 * "Use a new link" for a stopped download: paste a fresh link (an expired one, a mirror...). The app checks it is the
 * same file before keeping the saved bytes; anything less is reported, and nothing is changed until the user says.
 */
function NewLinkDialog({ id, onClose }: { id: number; onClose: () => void }) {
  const t = useT();
  const d = useDownload(id);
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<NewLinkOutcome | null>(null);
  const [error, setError] = useState<string | null>(null);
  const field = useRef<HTMLInputElement>(null);

  useEffect(() => field.current?.focus(), []);
  useEffect(() => {
    const esc = (e: KeyboardEvent) => e.key === "Escape" && !busy && onClose();
    document.addEventListener("keydown", esc);
    return () => document.removeEventListener("keydown", esc);
  }, [busy, onClose]);

  const check = async (mode: "auto" | "confirm") => {
    if (!url.trim()) return;
    setBusy(true);
    setError(null);
    try {
      const o = await api.useNewLink(id, { url: url.trim() }, mode);
      setResult(o);
      if (o.applied) setTimeout(onClose, 1200);
    } catch (e) {
      setResult(null);
      setError(normalizeError(e).message);
    }
    setBusy(false);
  };
  const addNew = async () => {
    setBusy(true);
    const ok = await act(() => api.createDownload({ url: url.trim() }), t("nl.added"));
    setBusy(false);
    if (ok) onClose();
  };
  const submit = (e: FormEvent) => {
    e.preventDefault();
    void check("auto");
  };
  if (!d) return null;
  const offers = result ? outcomeView(result).offers : [];

  return createPortal(
    <div
      className="fixed inset-0 z-40 grid place-items-center bg-black/45 p-4"
      onClick={(e) => {
        e.stopPropagation();
        if (e.target === e.currentTarget && !busy) onClose();
      }}
      onContextMenu={(e) => e.stopPropagation()}
    >
      <form className="card rise w-full max-w-md space-y-4 p-5 shadow-xl" role="dialog" aria-modal="true" aria-labelledby="newlink-title" data-testid="newlink-dialog" onSubmit={submit}>
        <div className="flex items-center gap-3">
          <div className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-accent-soft text-accent">
            <Icon name="link" size={19} />
          </div>
          <div className="min-w-0">
            <h2 id="newlink-title" className="text-[15px] font-semibold">
              {t("nl.title")}
            </h2>
            <p className="truncate text-xs text-dim">{d.filename}</p>
          </div>
        </div>
        <p className="text-xs text-dim">{t("nl.intro")}</p>
        <label className="block space-y-1.5">
          <span className="label">{t("nl.link")}</span>
          <input
            ref={field}
            className="input"
            data-testid="newlink-url"
            value={url}
            onChange={(e) => {
              setUrl(e.target.value);
              setResult(null);
            }}
            placeholder="https://"
            autoComplete="off"
            spellCheck={false}
            disabled={busy}
          />
        </label>
        {busy && (
          <p className="text-xs text-dim" role="status" data-testid="newlink-checking">
            {t("nl.checking")}
          </p>
        )}
        {result && <NewLinkResult o={result} />}
        {error && (
          <p className="text-xs text-bad" role="alert" data-testid="newlink-error">
            {error}
          </p>
        )}
        <div className="flex flex-wrap justify-end gap-2">
          <button type="button" className="btn" onClick={onClose} disabled={busy}>
            {result?.applied ? t("common.close") : t("common.cancel")}
          </button>
          {offers.includes("addNew") && (
            <button type="button" className="btn" data-testid="newlink-add" disabled={busy} onClick={() => void addNew()}>
              {t("nl.addNew")}
            </button>
          )}
          {offers.includes("confirm") ? (
            <button type="button" className="btn btn-primary" data-testid="newlink-confirm" disabled={busy} onClick={() => void check("confirm")}>
              {t("nl.confirm")}
            </button>
          ) : (
            !result?.applied && (
              <button type="submit" className="btn btn-primary" data-testid="newlink-check" disabled={busy || !url.trim()}>
                {t("nl.check")}
              </button>
            )
          )}
        </div>
      </form>
    </div>,
    document.body,
  );
}

/** Mounted once: opens the dialog for the download named by a `velox:new-link` event. */
export function NewLinkHost() {
  const [id, setId] = useState<number | null>(null);
  useEffect(() => {
    const open = (e: Event) => setId((e as CustomEvent<number>).detail);
    window.addEventListener(NEW_LINK, open);
    return () => window.removeEventListener(NEW_LINK, open);
  }, []);
  return id === null ? null : <NewLinkDialog key={id} id={id} onClose={() => setId(null)} />;
}
