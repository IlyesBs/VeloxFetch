import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { actions } from "../actions";
import { api, normalizeError } from "../api";
import { canOpen, FileIcon, kindLabel, KINDS } from "../components/FileIcon";
import { formatBytes, formatRelative, hostOf } from "../format";
import { act, store, useStore } from "../hooks";
import type { FileKind, HistoryEntry } from "../types";
import { Icon } from "../ui/icons";
import { t as tr, useT } from "../i18n";
import { EmptyState, Menu } from "../ui/primitives";
import { FOCUS_SEARCH } from "./DownloadsView";

/** "Today", "Yesterday", "Earlier this week", "Earlier" for a unix time. */
export function dayGroup(unixSecs: number, now = new Date()): string {
  const d = new Date(unixSecs * 1000);
  const start = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const t = d.getTime();
  if (t >= start) return tr("hist.today");
  if (t >= start - 86_400_000) return tr("hist.yesterday");
  if (t >= start - 6 * 86_400_000) return tr("hist.week");
  return tr("hist.earlier");
}

function Row({ h, onRemoved }: { h: HistoryEntry; onRemoved: (id: number) => void }) {
  const t = useT();
  const target = { kind: "history", id: h.id } as const;
  const openable = h.exists && canOpen(h.filename, h.kind);
  return (
    <li className="card card-hover flex items-center gap-3 p-3" data-testid="history-row" data-exists={h.exists}>
      <FileIcon kind={h.kind} size={36} />
      <div className="min-w-0 flex-1">
        <p className="truncate text-[13px] font-semibold" title={h.destination}>
          {h.filename}
        </p>
        <p className="truncate text-xs text-dim">
          {hostOf(h.url)}
          {!h.exists && <span className="ms-2 rounded-full bg-line px-1.5 py-px text-[10px] font-medium">{t("hist.moved")}</span>}
        </p>
      </div>
      <span className="num hidden text-xs text-dim sm:inline">{formatBytes(h.size)}</span>
      <span className="num hidden w-20 text-end text-xs text-dim sm:inline">{formatRelative(h.finishedAt)}</span>
      <div className="flex items-center gap-1">
        {openable ? (
          <button className="btn btn-ghost btn-icon" aria-label={t("hist.openName", { name: h.filename })} title={t("mj.open")} data-testid="history-open" onClick={() => void actions.openFile(target)}>
            <Icon name="external" size={15} />
          </button>
        ) : (
          <span className="w-8" aria-hidden="true" />
        )}
        <button
          className="btn btn-ghost btn-icon"
          aria-label={t("hist.showName", { name: h.filename })}
          title={t("mj.showFolder")}
          data-testid="history-show"
          onClick={() => void actions.showInFolder(target)}
        >
          <Icon name="folder" size={15} />
        </button>
        <Menu
          label={t("hist.more", { name: h.filename })}
          items={[
            { label: t("dup.again"), icon: "retry", onSelect: () => void actions.downloadAgain(h.url) },
            { label: t("done.copy"), icon: "copy", onSelect: () => void actions.copy(h.url) },
            {
              label: t("hist.remove"),
              icon: "trash",
              danger: true,
              onSelect: async () => {
                if ((await act(() => api.forgetHistory(h.id))) !== undefined) onRemoved(h.id);
              },
            },
          ]}
        />
      </div>
    </li>
  );
}

export function HistoryView() {
  const t = useT();
  const [rows, setRows] = useState<HistoryEntry[] | null>(null);
  const [query, setQuery] = useState("");
  const [kind, setKind] = useState<FileKind | "all">("all");
  const [confirmClear, setConfirmClear] = useState(false);
  const completed = useStore((s) => s.metrics.completed);
  const search = useRef<HTMLInputElement>(null);

  // Reload on open and whenever a download finishes.
  useEffect(() => {
    let alive = true;
    api
      .listHistory(500)
      .then((r) => alive && setRows(r))
      .catch((e) => store.toast("error", normalizeError(e).message));
    return () => {
      alive = false;
    };
  }, [completed]);

  useEffect(() => {
    const focus = () => search.current?.focus();
    window.addEventListener(FOCUS_SEARCH, focus);
    return () => window.removeEventListener(FOCUS_SEARCH, focus);
  }, []);

  const removed = useCallback((id: number) => setRows((r) => r?.filter((h) => h.id !== id) ?? r), []);
  const clear = async () => {
    setConfirmClear(false);
    const n = await act(() => api.forgetHistory(null));
    if (n !== undefined) {
      setRows([]);
      store.toast("info", `History cleared (${n} line${n === 1 ? "" : "s"}); your files were not touched`);
    }
  };

  const q = query.trim().toLowerCase();
  const shown = useMemo(
    () => (rows ?? []).filter((h) => (kind === "all" || h.kind === kind) && (!q || h.filename.toLowerCase().includes(q) || h.url.toLowerCase().includes(q))),
    [rows, kind, q],
  );
  const kindsPresent = useMemo(() => KINDS.filter((k) => rows?.some((h) => h.kind === k)), [rows]);
  const grouped = useMemo(() => {
    const out: { title: string; items: HistoryEntry[] }[] = [];
    for (const h of shown) {
      const title = dayGroup(h.finishedAt);
      const last = out[out.length - 1];
      if (last?.title === title) last.items.push(h);
      else out.push({ title, items: [h] });
    }
    return out;
  }, [shown]);

  if (rows === null) return <p className="text-dim">{t("common.loading")}</p>;
  if (rows.length === 0) {
    return <EmptyState icon="history" title={t("history.emptyTitle")} body={t("history.emptyBody")} />;
  }
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <div className="relative w-full sm:w-56">
          <Icon name="search" size={15} className="pointer-events-none absolute start-2.5 top-1/2 -translate-y-1/2 text-dim" />
          <input
            ref={search}
            className="input h-8 ps-8"
            placeholder={t("hist.search")}
            aria-label={t("hist.search")}
            data-testid="history-search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && setQuery("")}
            spellCheck={false}
          />
        </div>
        {kindsPresent.length > 1 && (
          <div role="group" aria-label={t("hist.kind")} className="flex flex-wrap gap-1.5">
            <button className="pill" aria-pressed={kind === "all"} onClick={() => setKind("all")}>
              {t("filter.all")}
            </button>
            {kindsPresent.map((k) => (
              <button key={k} className="pill" aria-pressed={kind === k} data-testid={`history-kind-${k}`} onClick={() => setKind(k)}>
                {kindLabel(k)}
              </button>
            ))}
          </div>
        )}
        <div className="ms-auto flex items-center gap-1.5">
          {confirmClear ? (
            <>
              <span className="text-xs text-dim">{t("hist.clearAsk", { n: rows.length })}</span>
              <button className="btn h-7 text-xs" onClick={() => setConfirmClear(false)}>
                {t("hist.keep")}
              </button>
              <button className="btn h-7 border-bad/40 text-xs text-bad" data-testid="history-clear-confirm" onClick={() => void clear()}>
                {t("hist.clear")}
              </button>
            </>
          ) : (
            <button className="btn h-7 text-xs" data-testid="history-clear" onClick={() => setConfirmClear(true)}>
              <Icon name="broom" size={13} /> {t("hist.clearAll")}
            </button>
          )}
        </div>
      </div>

      {shown.length === 0 ? (
        <p className="py-6 text-center text-dim">{t("hist.nothing")}</p>
      ) : (
        <div className="space-y-5" data-testid="history-list">
          {grouped.map((g) => (
            <section key={g.title} className="space-y-2">
              <h2 className="label">{g.title}</h2>
              <ul className="space-y-2">
                {g.items.map((h) => (
                  <Row key={h.id} h={h} onRemoved={removed} />
                ))}
              </ul>
            </section>
          ))}
        </div>
      )}
    </div>
  );
}
