import { useEffect, useMemo, useRef, useState } from "react";
import { actions } from "../actions";
import { AddDownload } from "../components/AddDownload";
import { BentoCards } from "../components/BentoCards";
import { DownloadCard } from "../components/DownloadCard";
import { MediaJobs } from "../components/MediaJobs";
import { SidePanels } from "../components/SidePanels";
import { ThrottleHud } from "../components/ThrottleHud";
import { hostOf } from "../format";
import { store, useStore } from "../hooks";
import type { GroupName } from "../store";
import type { Download } from "../types";
import { useT, type Key } from "../i18n";
import { setPrefs, usePrefs, type SortOrder } from "../prefs";
import { Icon } from "../ui/icons";
import { EmptyState } from "../ui/primitives";

export type Filter = "all" | "active" | "queued" | "paused" | "failed" | "completed";

const FILTERS: { name: Filter; label: Key; groups: GroupName[] }[] = [
  { name: "all", label: "filter.all", groups: ["active", "queued", "paused", "failed", "completed"] },
  { name: "active", label: "filter.active", groups: ["active"] },
  { name: "queued", label: "filter.queued", groups: ["queued"] },
  { name: "paused", label: "filter.paused", groups: ["paused"] },
  { name: "completed", label: "filter.completed", groups: ["completed"] },
  { name: "failed", label: "filter.failed", groups: ["failed"] },
];

const SORTS: { name: SortOrder; label: Key }[] = [
  { name: "status", label: "sort.status" },
  { name: "newest", label: "sort.newest" },
  { name: "name", label: "sort.name" },
  { name: "size", label: "sort.size" },
  { name: "progress", label: "sort.progress" },
];

/** In "All", finished downloads beyond this many are behind "Show all N finished" (the list stays light). */
const ALL_FINISHED_SHOWN = 30;

function matches(d: Download | undefined, q: string): boolean {
  if (!d) return false;
  if (!q) return true;
  const needle = q.toLowerCase();
  return d.filename.toLowerCase().includes(needle) || hostOf(d.url).toLowerCase().includes(needle) || d.url.toLowerCase().includes(needle);
}

/** `ids` in the chosen order. "status" keeps the engine's grouping (running, waiting, paused, failed, finished). */
function sorted(ids: number[], order: SortOrder, byId: Record<number, Download>): number[] {
  if (order === "status") return ids;
  const get = (id: number) => byId[id]!;
  const by: Record<Exclude<SortOrder, "status">, (a: number, b: number) => number> = {
    newest: (a, b) => b - a,
    name: (a, b) => get(a).filename.localeCompare(get(b).filename),
    size: (a, b) => (get(b).totalSize ?? get(b).downloaded) - (get(a).totalSize ?? get(a).downloaded),
    progress: (a, b) => {
      const p = (d: Download) => (d.status === "completed" ? 1 : d.totalSize ? d.downloaded / d.totalSize : 0);
      return p(get(b)) - p(get(a));
    },
  };
  return [...ids].sort(by[order]);
}

/** Focus the search box of the current view (Ctrl+F). */
export const FOCUS_SEARCH = "velox:focus-search";

/**
 * The dashboard (reference layout): the link bar, the row of six cards, then the tabbed list of downloads
 * beside a side column (network, disk, upcoming, quick actions). On narrower pages the side column moves
 * under the list; the Smart throttle card comes here while the sidebar is folded.
 */
export function DownloadsView({
  onOpen,
  onHistory,
  onBatch,
  onStream,
  onQueue,
  onSettings,
  filter,
  onFilter,
  showHud,
}: {
  onOpen: (id: number) => void;
  onHistory: () => void;
  onBatch: () => void;
  onStream: (url: string) => void;
  onQueue: () => void;
  onSettings: () => void;
  filter: Filter;
  onFilter: (f: Filter) => void;
  showHud: boolean;
}) {
  const t = useT();
  const groups = useStore((s) => s.groups);
  const ready = useStore((s) => s.ready);
  const streams = useStore((s) => s.media.length);
  const { density, sort } = usePrefs();
  const [query, setQuery] = useState("");
  const [sortOpen, setSortOpen] = useState(false);
  const search = useRef<HTMLInputElement>(null);
  const sortBox = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const focus = () => search.current?.focus();
    window.addEventListener(FOCUS_SEARCH, focus);
    return () => window.removeEventListener(FOCUS_SEARCH, focus);
  }, []);

  useEffect(() => {
    if (!sortOpen) return;
    const close = () => setSortOpen(false);
    const away = (e: Event) => !sortBox.current?.contains(e.target as Node) && close();
    const esc = (e: KeyboardEvent) => e.key === "Escape" && close();
    document.addEventListener("pointerdown", away, true);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("pointerdown", away, true);
      document.removeEventListener("keydown", esc);
    };
  }, [sortOpen]);

  const total = groups.active.length + groups.queued.length + groups.paused.length + groups.failed.length + groups.completed.length;
  const nothing = ready && total === 0 && streams === 0;
  const q = query.trim();
  const filtering = filter !== "all" || q !== "";

  // The list. Recomputed on status changes, typing and order changes only (not on progress ticks, except
  // for the progress order, which reads the current numbers when the list is rebuilt).
  const { shown, hiddenFinished } = useMemo(() => {
    const byId = store.getState().byId;
    const def = FILTERS.find((f) => f.name === filter)!;
    if (filter === "all" && !q) {
      // running, waiting, paused, failed, then the newest finished ones
      const finished = [...groups.completed].reverse();
      const ids = [...groups.active, ...groups.queued, ...groups.paused, ...groups.failed, ...finished.slice(0, ALL_FINISHED_SHOWN)];
      return { shown: sorted(ids, sort, byId), hiddenFinished: Math.max(0, finished.length - ALL_FINISHED_SHOWN) };
    }
    const ids = def.groups.flatMap((g) => (g === "completed" ? [...groups[g]].reverse() : groups[g])).filter((id) => matches(byId[id], q));
    return { shown: sorted(ids, sort, byId), hiddenFinished: 0 };
  }, [filter, q, groups, sort]);

  const list = (ids: number[]) => ids.map((id) => store.getState().byId[id]).filter((d): d is Download => !!d);
  const count = (f: (typeof FILTERS)[number]) => f.groups.reduce((n, g) => n + groups[g].length, 0);

  return (
    <div className="@container">
      <div className="grid gap-4 @[1040px]:grid-cols-[minmax(0,1fr)_18.5rem]">
        <div className="min-w-0 @[1040px]:col-start-1">
          <AddDownload onBatch={onBatch} onStream={onStream} onOpen={onOpen} />
        </div>
        <div className="min-w-0 @[1040px]:col-span-2">
          <BentoCards />
        </div>
        {showHud && (
          <div className="min-w-0 @[1040px]:col-span-2">
            <ThrottleHud variant="wide" />
          </div>
        )}

        <div className="min-w-0 space-y-4">
          {nothing && <EmptyState icon="download" title={t("empty.title")} body={t("empty.body")} />}

          <MediaJobs />

          {total > 0 && (
            <div className="panel flex flex-wrap items-center gap-2 p-1.5" data-testid="list-toolbar">
              <div role="group" aria-label={t("list.tabs")} className="tabbar grow basis-auto">
                {FILTERS.map((f) =>
                  // "Waiting" only when something waits (the queue has its own page); the others always
                  f.name === "queued" && count(f) === 0 && filter !== f.name ? null : (
                    <button key={f.name} className="tab" aria-pressed={filter === f.name} data-testid={`filter-${f.name}`} onClick={() => onFilter(f.name)}>
                      {t(f.label)}
                      <span className="num">({count(f)})</span>
                    </button>
                  ),
                )}
              </div>
              <div className="flex min-w-0 flex-1 items-center justify-end gap-1 @[700px]:flex-none">
                <div className="relative min-w-0 flex-1 @[700px]:w-48 @[700px]:flex-none">
                  <Icon name="search" size={17} className="pointer-events-none absolute start-3 top-1/2 -translate-y-1/2 text-dim" />
                  <input
                    ref={search}
                    className="input h-10 rounded-xl bg-transparent ps-10"
                    placeholder={t("list.search")}
                    aria-label={t("list.search")}
                    data-testid="list-search"
                    value={query}
                    onChange={(e) => setQuery(e.target.value)}
                    onKeyDown={(e) => e.key === "Escape" && setQuery("")}
                    spellCheck={false}
                  />
                </div>
                <div role="group" aria-label={t("list.view")} className="flex">
                  <button className="tool-btn" aria-pressed={density === "cards"} aria-label={t("list.cards")} title={t("list.cards")} data-testid="view-cards" onClick={() => setPrefs({ density: "cards" })}>
                    <Icon name="activeBox" size={19} />
                  </button>
                  <button className="tool-btn" aria-pressed={density === "compact"} aria-label={t("list.compact")} title={t("list.compact")} data-testid="view-compact" onClick={() => setPrefs({ density: "compact" })}>
                    <Icon name="queue" size={19} />
                  </button>
                </div>
                <div className="relative" ref={sortBox}>
                  <button className="tool-btn" aria-haspopup="true" aria-expanded={sortOpen} aria-label={t("list.sort")} title={t("list.sort")} data-testid="sort-open" onClick={() => setSortOpen(!sortOpen)}>
                    <Icon name="sliders" size={19} />
                  </button>
                  <div role="radiogroup" aria-label={t("list.sort")} className="popover end-0 top-12" hidden={!sortOpen}>
                    {SORTS.map((s) => (
                      <button
                        key={s.name}
                        role="radio"
                        aria-checked={sort === s.name}
                        className="popover-item"
                        data-testid={`sort-${s.name}`}
                        onClick={() => {
                          setPrefs({ sort: s.name });
                          setSortOpen(false);
                        }}
                      >
                        <span className="flex-1">{t(s.label)}</span>
                        {sort === s.name && <Icon name="check" size={14} />}
                      </button>
                    ))}
                  </div>
                </div>
              </div>
            </div>
          )}

          {total > 0 && (groups.paused.length > 1 || groups.failed.length > 1 || groups.completed.length > 0) && (
            <div className="flex flex-wrap justify-end gap-1.5">
              {groups.paused.length > 1 && (
                <button className="btn h-8 text-xs" data-testid="bulk-resume" onClick={() => void actions.resumeAll(list(groups.paused))}>
                  <Icon name="play" size={13} /> {t("bulk.resume")}
                </button>
              )}
              {groups.failed.length > 1 && (
                <button className="btn h-8 text-xs" data-testid="bulk-retry" onClick={() => void actions.retryAll(list(groups.failed))}>
                  <Icon name="retry" size={13} /> {t("bulk.retry")}
                </button>
              )}
              {groups.completed.length > 0 && (
                <button className="btn h-8 text-xs" data-testid="bulk-clear" title={t("bulk.clearTip")} onClick={() => void actions.clearCompleted(list(groups.completed))}>
                  <Icon name="broom" size={13} /> {t("bulk.clear")}
                </button>
              )}
              <button className="btn h-8 text-xs" onClick={onHistory}>
                <Icon name="history" size={13} /> {t("list.history")}
              </button>
            </div>
          )}

          {total > 0 &&
            (shown.length > 0 ? (
              <div className="space-y-3" data-testid={filtering ? "filtered-list" : "all-list"}>
                {shown.map((id) => (
                  <DownloadCard key={id} id={id} onOpen={onOpen} density={density} />
                ))}
              </div>
            ) : (
              <p className="py-6 text-center text-dim" data-testid="filtered-empty">
                {t("list.nothing")}
                {q ? ` “${q}”` : ""}.{" "}
                <button
                  className="font-medium text-accent hover:underline"
                  onClick={() => {
                    setQuery("");
                    onFilter("all");
                  }}
                >
                  {t("list.showAll")}
                </button>
              </p>
            ))}
          {hiddenFinished > 0 && (
            <div className="text-center">
              <button className="btn" data-testid="show-all-finished" onClick={() => onFilter("completed")}>
                {t("list.moreFinished", { n: groups.completed.length })}
              </button>
            </div>
          )}
        </div>

        <aside className="grid min-w-0 content-start gap-4 @[640px]:grid-cols-2 @[1040px]:grid-cols-1" aria-label={t("side.quick")} data-testid="dash-side">
          <SidePanels onOpen={onOpen} onQueue={onQueue} onBatch={onBatch} onStream={onStream} onSettings={onSettings} />
        </aside>
      </div>
    </div>
  );
}
