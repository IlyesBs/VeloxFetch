import { useEffect, useRef, useState } from "react";
import { api, normalizeError } from "../api";
import { act, store } from "../hooks";
import { t as tr, useT } from "../i18n";
import { Rich } from "../ui/rich";
import type { BatchItem, BatchPreview, BatchRequest } from "../types";

const MAX_FILE_BYTES = 512 * 1024;

type Mode = "links" | "pattern";

function badge(item: BatchItem): { text: string; cls: string } {
  switch (item.state.kind) {
    case "ok":
      return { text: tr("batch.willAdd"), cls: "text-ok" };
    case "duplicateInBatch":
      return { text: tr("batch.repeated"), cls: "text-warn" };
    case "alreadyInList":
      return { text: tr("dup.title"), cls: "text-warn" };
    case "invalid":
      return { text: item.state.reason, cls: "text-bad" };
  }
}

/**
 * Add many downloads at once: a list of links (typed, pasted, or imported from a .txt file), or a
 * pattern such as https://host/file_[001-100].zip. A live preview shows exactly what would be added
 * (duplicates and problems are called out), nothing is added until the user presses the button, and
 * every rule (limits, link validation, folder validation) is enforced again in Rust.
 */
export function BatchDialog({ initialLinks, onClose }: { initialLinks: string; onClose: () => void }) {
  const t = useT();
  const [mode, setMode] = useState<Mode>("links");
  const [links, setLinks] = useState(initialLinks);
  const [pattern, setPattern] = useState("");
  const [subdir, setSubdir] = useState("");
  const [start, setStart] = useState(true);
  const [preview, setPreview] = useState<BatchPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const file = useRef<HTMLInputElement>(null);

  const request = (): BatchRequest => ({
    ...(mode === "links" ? { links } : { pattern }),
    ...(subdir.trim() && { subdir: subdir.trim() }),
    start,
  });
  const source = mode === "links" ? links : pattern;

  // Live preview, a moment after the user stops typing. Out-of-date answers are dropped.
  useEffect(() => {
    if (!source.trim()) {
      setPreview(null);
      setError(null);
      return;
    }
    let current = true;
    const timer = setTimeout(() => {
      api.previewBatch(request()).then(
        (p) => {
          if (!current) return;
          setPreview(p);
          setError(null);
        },
        (e) => {
          if (!current) return;
          setPreview(null);
          setError(normalizeError(e).message);
        },
      );
    }, 300);
    return () => {
      current = false;
      clearTimeout(timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mode, links, pattern, subdir]);

  useEffect(() => {
    const esc = (e: KeyboardEvent) => e.key === "Escape" && !busy && onClose();
    document.addEventListener("keydown", esc);
    return () => document.removeEventListener("keydown", esc);
  }, [busy, onClose]);

  const importFile = async (f: File | undefined) => {
    if (!f) return;
    if (f.size > MAX_FILE_BYTES) {
      setError(t("batch.tooBig", { kib: MAX_FILE_BYTES / 1024 }));
      return;
    }
    setMode("links");
    setLinks(await f.text());
  };

  const add = async () => {
    setBusy(true);
    const r = await act(() => api.addBatch(request()));
    setBusy(false);
    if (!r) return;
    const skipped = r.duplicates + r.existing + r.invalid;
    store.toast(
      r.failed > 0 ? "error" : "info",
      t("batch.added", { n: r.added }) +
        (skipped > 0
          ? `, ${t("batch.skipped", {
              n: skipped,
              why: [r.duplicates && t("batch.nRepeated", { n: r.duplicates }), r.existing && t("batch.nExisting", { n: r.existing }), r.invalid && t("batch.nInvalid", { n: r.invalid })]
                .filter(Boolean)
                .join(", "),
            })}`
          : "") +
        (r.failed > 0 ? `; ${t("batch.nFailed", { n: r.failed })}` : ""),
    );
    onClose();
  };

  const canAdd = !!preview && preview.ok > 0 && !busy;

  return (
    <div className="fixed inset-0 z-40 grid place-items-center bg-black/45 p-4" data-testid="batch-overlay">
      <div className="card rise flex max-h-[92vh] w-full max-w-2xl flex-col p-5 shadow-xl" role="dialog" aria-modal="true" aria-labelledby="batch-title" data-testid="batch-dialog">
        <h2 id="batch-title" className="text-[15px] font-semibold">
          {t("batch.title")}
        </h2>

        <div role="tablist" className="mt-3 inline-flex self-start rounded-xl border border-line bg-raised p-1">
          {(["links", "pattern"] as const).map((m) => (
            <button
              key={m}
              role="tab"
              aria-selected={mode === m}
              data-testid={`batch-tab-${m}`}
              onClick={() => setMode(m)}
              className={`h-8 rounded-lg px-3 text-[13px] font-medium transition-colors ${mode === m ? "bg-surface text-fg shadow-sm" : "text-dim hover:text-fg"}`}
            >
              {m === "links" ? t("batch.list") : t("batch.pattern")}
            </button>
          ))}
        </div>

        {mode === "links" ? (
          <div className="mt-3">
            <textarea
              data-testid="batch-links"
              className="input min-h-32 w-full font-mono text-xs"
              placeholder={t("batch.linksEg")}
              value={links}
              onChange={(e) => setLinks(e.target.value)}
              spellCheck={false}
              autoFocus
            />
            <div className="mt-1.5 flex items-center gap-2 text-xs text-dim">
              <input ref={file} type="file" accept=".txt,.text,text/plain" className="hidden" data-testid="batch-file" onChange={(e) => void importFile(e.target.files?.[0])} />
              <button type="button" className="btn h-7 px-2.5 text-xs" onClick={() => file.current?.click()}>
                {t("batch.import")}
              </button>
              <span>{t("batch.upTo")}</span>
            </div>
          </div>
        ) : (
          <div className="mt-3">
            <input
              data-testid="batch-pattern"
              className="input w-full font-mono text-xs"
              placeholder="https://example.com/file_[001-100].zip"
              value={pattern}
              onChange={(e) => setPattern(e.target.value)}
              spellCheck={false}
              autoFocus
            />
            <p className="mt-1.5 text-xs text-dim">
              <Rich text={t("batch.patternHow")} />
            </p>
          </div>
        )}

        <div className="mt-3 grid grid-cols-[1fr_auto] items-end gap-3">
          <label className="space-y-1">
            <span className="label">{t("mdl.subdir")}</span>
            <input data-testid="batch-subdir" className="input h-9" value={subdir} onChange={(e) => setSubdir(e.target.value)} placeholder={t("batch.subdirEg")} />
          </label>
          <label className="flex h-9 cursor-pointer items-center gap-2 text-[13px]">
            <input type="checkbox" data-testid="batch-start" checked={start} onChange={(e) => setStart(e.target.checked)} />
            {t("batch.startNow")}
          </label>
        </div>

        <div className="mt-3 min-h-0 flex-1 overflow-y-auto rounded-xl border border-line" data-testid="batch-preview">
          {error ? (
            <p className="p-3 text-xs text-bad" role="alert" data-testid="batch-error">
              {error}
            </p>
          ) : !preview ? (
            <p className="p-3 text-xs text-dim">{t("batch.empty")}</p>
          ) : (
            <>
              <p className="sticky top-0 border-b border-line bg-surface px-3 py-2 text-xs" data-testid="batch-summary">
                <span data-testid="batch-ok" hidden>
                  {preview.ok}
                </span>
                <Rich text={t("batch.sumOk", { n: preview.ok })} />
                {preview.duplicates > 0 && (
                  <>
                    {" "}
                    · <span className="text-warn">{t("batch.nRepeated", { n: preview.duplicates })}</span>
                  </>
                )}
                {preview.existing > 0 && (
                  <>
                    {" "}
                    · <span className="text-warn">{t("batch.nAlready", { n: preview.existing })}</span>
                  </>
                )}
                {preview.invalid > 0 && (
                  <>
                    {" "}
                    · <span className="text-bad">{t("batch.nInvalid", { n: preview.invalid })}</span>
                  </>
                )}
              </p>
              <ul className="divide-y divide-line text-xs">
                {preview.items.map((it, i) => {
                  const b = badge(it);
                  return (
                    <li key={i} className="flex items-baseline gap-3 px-3 py-1.5" title={it.url}>
                      <span className="w-8 shrink-0 text-end text-dim">{it.line}</span>
                      <span className="selectable min-w-0 flex-1 truncate">{it.filename || it.url}</span>
                      <span className={`shrink-0 ${b.cls}`}>{b.text}</span>
                    </li>
                  );
                })}
              </ul>
              {preview.truncated && <p className="px-3 py-2 text-xs text-dim">{t("batch.truncated", { n: preview.items.length })}</p>}
            </>
          )}
        </div>

        <div className="mt-4 flex items-center justify-end gap-2">
          <button className="btn" data-testid="batch-cancel" disabled={busy} onClick={onClose}>
            {t("common.cancel")}
          </button>
          <button className="btn btn-primary" data-testid="batch-add" disabled={!canAdd} onClick={() => void add()}>
            {preview && preview.ok > 0 ? (preview.ok === 1 ? t("batch.addOne") : t("batch.addN", { n: preview.ok })) : t("batch.add")}
          </button>
        </div>
      </div>
    </div>
  );
}
