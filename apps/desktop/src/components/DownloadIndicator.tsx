import { memo, useEffect, useId, useMemo, useRef, useState } from "react";
import { formatBytes, formatEta, formatSpeed } from "../format";
import { useStore } from "../hooks";
import { useT } from "../i18n";
import { indicatorItems, type IndicatorItem } from "../indicator";
import { Icon } from "../ui/icons";
import { ProgressBar } from "../ui/primitives";
import { FileIcon } from "./FileIcon";
import { openMediaDetail } from "./MediaJobs";

/** How long the bar takes to slide away once the last download ends (styles.css `.dl-ind.is-leaving`). */
const LEAVE_MS = 220;

/** "42% · 1:20 left · 3.1 MB/s", or "Verifying", or "12 MB · 3.1 MB/s" while the size is not known. */
function Numbers({ item }: { item: IndicatorItem }) {
  const t = useT();
  const parts: string[] = [];
  if (item.phase) parts.push(t(item.phase));
  parts.push(item.fraction === null ? formatBytes(item.bytes) : `${Math.floor(item.fraction * 100)}%`);
  if (item.etaSecs !== null) parts.push(t("md.left", { t: formatEta(item.etaSecs) }));
  if (item.speedBps > 0) parts.push(formatSpeed(item.speedBps));
  return (
    <span className="num truncate text-[11.5px] text-dim" data-testid="ind-numbers">
      {parts.join(" · ")}
    </span>
  );
}

/** One download: its icon, name, numbers and progress. Clicking it opens its details page. */
const Line = memo(function Line({ item, onOpen, first }: { item: IndicatorItem; onOpen: (id: number) => void; first?: boolean }) {
  const t = useT();
  return (
    <button
      type="button"
      className={`dl-ind-line ${first ? "" : "rise"}`}
      data-testid={first ? "ind-first" : `ind-${item.key}`}
      title={item.name}
      aria-label={t("md.openDetails", { title: item.name })}
      onClick={() => (item.kind === "media" ? openMediaDetail(item.id) : onOpen(item.id))}
    >
      <FileIcon kind={item.fileKind} size={32} />
      <span className="flex min-w-0 flex-1 flex-col gap-1">
        <span className="truncate text-[13px] font-semibold text-fg" data-testid="ind-name">
          {item.name}
        </span>
        <ProgressBar value={item.fraction ?? 0} live={item.fraction === null || item.phase !== null} label={t("mj.progress", { title: item.name })} />
        <Numbers item={item} />
      </span>
    </button>
  );
});

/**
 * A small bar at the bottom of the window while something downloads: the first download (its name, progress and
 * time left) always in view; with several, an arrow shows (and hides) all the others above it.
 */
export function DownloadIndicator({ onOpen }: { onOpen: (id: number) => void }) {
  const t = useT();
  const active = useStore((s) => s.groups.active);
  const byId = useStore((s) => s.byId);
  const media = useStore((s) => s.media);
  const items = useMemo(() => indicatorItems(active, byId, media), [active, byId, media]);
  const [open, setOpen] = useState(false);
  const listId = useId();

  // When the last download ends, the bar slides away showing what it showed, then goes.
  const last = useRef<IndicatorItem[]>(items);
  const [leaving, setLeaving] = useState(false);
  if (items.length > 0) last.current = items;
  const shown = items.length > 0 ? items : last.current;
  useEffect(() => {
    if (items.length > 0) return setLeaving(false);
    setLeaving(true);
    const timer = window.setTimeout(() => {
      last.current = [];
      setLeaving(false);
    }, LEAVE_MS);
    return () => window.clearTimeout(timer);
  }, [items.length]);

  // Nothing more to list: the list closes (and opens closed next time).
  const others = shown.slice(1);
  useEffect(() => {
    if (items.length <= 1) setOpen(false);
  }, [items.length]);

  const first = shown[0];
  if (!first || (items.length === 0 && !leaving)) return null;
  return (
    <section
      className={`dl-ind ${leaving && items.length === 0 ? "is-leaving" : ""} ${open ? "is-open" : ""}`}
      aria-label={t("ind.label")}
      data-testid="download-indicator"
      data-count={items.length}
    >
      {others.length > 0 && (
        <div className="dl-ind-list" id={listId} data-testid="ind-list" aria-hidden={!open} inert={!open}>
          <div className="dl-ind-list-inner">
            {others.map((it) => (
              <Line key={it.key} item={it} onOpen={onOpen} />
            ))}
          </div>
        </div>
      )}
      <div className="dl-ind-bar">
        <Line item={first} onOpen={onOpen} first />
        {others.length > 0 && (
          <button
            type="button"
            className="dl-ind-toggle"
            data-testid="ind-toggle"
            aria-expanded={open}
            aria-controls={listId}
            aria-label={open ? t("ind.hide") : t("ind.show", { n: shown.length })}
            title={open ? t("ind.hide") : t("ind.show", { n: shown.length })}
            onClick={() => setOpen(!open)}
          >
            <span className="num text-[11px] font-semibold">+{others.length}</span>
            <Icon name="chevronDown" size={15} className="dl-ind-chevron" />
          </button>
        )}
      </div>
    </section>
  );
}
