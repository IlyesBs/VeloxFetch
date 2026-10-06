import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { actions } from "../actions";
import { DeleteConfirm } from "../components/DeleteConfirm";
import { canOpen, kindLabel } from "../components/FileIcon";
import { MediaJobs } from "../components/MediaJobs";
import { formatBytes, hostOf } from "../format";
import { store, useDownload, useStore } from "../hooks";
import { useT, type Key } from "../i18n";
import type { Download, FileKind } from "../types";
import { Icon, type IconName } from "../ui/icons";
import { EmptyState } from "../ui/primitives";
import { FOCUS_SEARCH } from "./DownloadsView";

type Order = "newest" | "name" | "size";
const ORDERS: { name: Order; label: Key }[] = [
  { name: "newest", label: "sort.newest" },
  { name: "name", label: "sort.name" },
  { name: "size", label: "sort.size" },
];
const KINDS: FileKind[] = ["video", "audio", "image", "document", "archive", "program", "other"];
const KIND_ICON: Record<FileKind, IconName> = { video: "film", audio: "music", image: "image", document: "doc", archive: "archive", program: "app", other: "file" };

const size = (d: Download) => d.totalSize ?? d.downloaded;

/** One finished file: what it is, how big, where from, and everything that can be done with it. */
const DoneRow = memo(function DoneRow({ id, onOpen }: { id: number; onOpen: (id: number) => void }) {
  const t = useT();
  const d = useDownload(id);
  const [confirm, setConfirm] = useState(false);
  const close = useCallback(() => setConfirm(false), []);
  if (!d) return null;
  const target = { kind: "download", id: d.id } as const;
  const ext = /\.([a-z0-9]{1,5})$/i.exec(d.filename)?.[1];
  return (
    <li className="done-row" data-testid={`done-${d.id}`}>
      {confirm && <DeleteConfirm name={d.filename} onDelete={() => void actions.deleteFile(d)} onRemove={() => void actions.remove(d.id, false)} onClose={close} />}
      <div className={`done-icon kind-${d.kind}`} title={kindLabel(d.kind)}>
        <Icon name={KIND_ICON[d.kind]} size={20} strokeWidth={1.6} />
      </div>
      <div className="min-w-0 flex-1">
        <button className="block max-w-full truncate text-start text-[14px] font-semibold hover:underline" title={d.filename} onClick={() => onOpen(d.id)}>
          {d.filename}
        </button>
        <p className="flex min-w-0 items-center gap-1.5 text-[12px] text-dim">
          <Icon name="checkCircle" size={13} className="shrink-0 text-ok" />
          <span className="num ltr shrink-0">{formatBytes(size(d))}</span>
          {ext && <span className="shrink-0 rounded px-1 text-[10px] font-bold uppercase ring-1 ring-line-strong">{ext}</span>}
          <span aria-hidden="true">•</span>
          <span className="ltr truncate" title={d.url}>
            {d.category ?? hostOf(d.url)}
          </span>
        </p>
      </div>
      <div className="flex shrink-0 items-center gap-1.5">
        {canOpen(d.filename, d.kind) && (
          <button className="round-btn round-primary" aria-label={t("done.open")} title={t("done.open")} data-testid="done-open" onClick={() => void actions.openFile(target)}>
            <Icon name="play" size={16} />
          </button>
        )}
        <button className="round-btn" aria-label={t("done.folder")} title={t("done.folder")} data-testid="done-folder" onClick={() => void actions.showInFolder(target)}>
          <Icon name="folder" size={16} />
        </button>
        <button className="round-btn hidden sm:grid" aria-label={t("done.copy")} title={t("done.copy")} data-testid="done-copy" onClick={() => void actions.copy(d.url).then((ok) => ok && store.toast("info", t("done.copied")))}>
          <Icon name="copy" size={16} />
        </button>
        <button className="round-btn" aria-label={t("done.remove")} title={t("done.remove")} data-testid="done-remove" onClick={() => void actions.remove(d.id, false)}>
          <Icon name="x" size={16} />
        </button>
        <button className="round-btn round-danger" aria-label={t("done.delete")} title={t("done.delete")} data-testid="done-delete" onClick={() => setConfirm(true)}>
          <Icon name="trash" size={16} />
        </button>
      </div>
    </li>
  );
});

/**
 * Finished downloads only, as a file list: search, the kind of file, the order; each file opens, shows in
 * its folder, leaves the list, or is deleted from the disk (after asking). Finished videos follow.
 */
export function CompletedView({ onOpen }: { onOpen: (id: number) => void }) {
  const t = useT();
  const completed = useStore((s) => s.groups.completed);
  const videos = useStore((s) => s.media.filter((j) => j.status === "completed").length);
  const [query, setQuery] = useState("");
  const [kind, setKind] = useState<FileKind | "all">("all");
  const [order, setOrder] = useState<Order>("newest");
  const search = useRef<HTMLInputElement>(null);
  const total = useStore((s) => s.groups.completed.reduce((n, id) => n + (s.byId[id] ? size(s.byId[id]) : 0), 0));

  useEffect(() => {
    const focus = () => search.current?.focus();
    window.addEventListener(FOCUS_SEARCH, focus);
    return () => window.removeEventListener(FOCUS_SEARCH, focus);
  }, []);

  const byKind = useMemo(() => {
    const byId = store.getState().byId;
    const n: Partial<Record<FileKind, number>> = {};
    for (const id of completed) {
      const k = byId[id]?.kind;
      if (k) n[k] = (n[k] ?? 0) + 1;
    }
    return n;
  }, [completed]);

  const shown = useMemo(() => {
    const byId = store.getState().byId;
    const q = query.trim().toLowerCase();
    const ids = [...completed].reverse().filter((id) => {
      const d = byId[id];
      if (!d || (kind !== "all" && d.kind !== kind)) return false;
      return !q || d.filename.toLowerCase().includes(q) || d.url.toLowerCase().includes(q);
    });
    if (order === "name") ids.sort((a, b) => byId[a]!.filename.localeCompare(byId[b]!.filename));
    if (order === "size") ids.sort((a, b) => size(byId[b]!) - size(byId[a]!));
    return ids;
  }, [completed, query, kind, order]);

  const clear = () => void actions.clearCompleted(completed.map((id) => store.getState().byId[id]).filter((d): d is Download => !!d));
  const nothing = completed.length === 0 && videos === 0;

  return (
    <div className="space-y-5" data-testid="completed-view">
      <header className="flex flex-wrap items-end gap-3 pt-2">
        <div className="min-w-0 flex-1">
          <h2 className="display text-[24px] font-semibold">{t("done.title")}</h2>
          <p className="num text-[13px] text-dim" data-testid="done-summary">
            {t("done.sub", { n: completed.length, size: formatBytes(total) })}
          </p>
        </div>
        {completed.length > 0 && (
          <button className="btn h-9" title={t("done.clearTip")} data-testid="done-clear" onClick={clear}>
            <Icon name="broom" size={14} /> {t("done.clearList")}
          </button>
        )}
      </header>

      {nothing && <EmptyState icon="checkCircle" title={t("done.empty")} body={t("done.emptyBody")} />}

      {completed.length > 0 && (
        <>
          <div className="panel flex flex-wrap items-center gap-2 p-1.5" data-testid="done-toolbar">
            <div role="group" aria-label={t("done.kinds")} className="tabbar grow basis-auto">
              <button className="tab" aria-pressed={kind === "all"} data-testid="kind-all" onClick={() => setKind("all")}>
                {t("filter.all")} <span className="num">({completed.length})</span>
              </button>
              {KINDS.filter((k) => byKind[k]).map((k) => (
                <button key={k} className="tab" aria-pressed={kind === k} data-testid={`kind-${k}`} onClick={() => setKind(k)}>
                  <Icon name={KIND_ICON[k]} size={14} /> {kindLabel(k)} <span className="num">({byKind[k]})</span>
                </button>
              ))}
            </div>
            <div className="flex min-w-0 flex-1 items-center justify-end gap-1 sm:flex-none">
              <div className="relative min-w-0 flex-1 sm:w-52 sm:flex-none">
                <Icon name="search" size={17} className="pointer-events-none absolute start-3 top-1/2 -translate-y-1/2 text-dim" />
                <input
                  ref={search}
                  className="input h-10 rounded-xl bg-transparent ps-10"
                  placeholder={t("done.search")}
                  aria-label={t("done.search")}
                  data-testid="done-search"
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  onKeyDown={(e) => e.key === "Escape" && setQuery("")}
                  spellCheck={false}
                />
              </div>
              <select className="input h-10 w-auto rounded-xl bg-transparent" aria-label={t("list.sort")} data-testid="done-sort" value={order} onChange={(e) => setOrder(e.target.value as Order)}>
                {ORDERS.map((o) => (
                  <option key={o.name} value={o.name}>
                    {t(o.label)}
                  </option>
                ))}
              </select>
            </div>
          </div>

          {shown.length > 0 ? (
            <ul className="space-y-2" data-testid="done-list">
              {shown.map((id) => (
                <DoneRow key={id} id={id} onOpen={onOpen} />
              ))}
            </ul>
          ) : (
            <p className="py-6 text-center text-dim" data-testid="done-nothing">
              {t("list.nothing")}
              {query.trim() ? ` “${query.trim()}”` : ""}.
            </p>
          )}
        </>
      )}

      {videos > 0 && <MediaJobs only="done" />}
    </div>
  );
}

