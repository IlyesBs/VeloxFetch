import { memo, useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api } from "../api";
import { windowControl } from "../chrome";
import { formatBytes, formatEta, formatSpeed } from "../format";
import { useStore } from "../hooks";
import { useT } from "../i18n";
import { indicatorItems, type IndicatorItem } from "../indicator";
import { Icon } from "../ui/icons";
import { ProgressBar } from "../ui/primitives";
import { FileIcon } from "./FileIcon";

/** How long the bar takes to slide away once the last download ends (styles.css `.dl-ind.is-leaving`). */
const LEAVE_MS = 220;
/** How long the list takes to fold (styles.css `.dl-ind-list`): the window gets smaller after that. */
const FOLD_MS = 300;
/** A press that moves this far (CSS pixels) moves the bar instead of clicking. */
const DRAG_PX = 4;

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

/** One download: its icon, name, numbers and progress. Clicking it opens its details page in the main window. */
const Line = memo(function Line({ item, first, dragged }: { item: IndicatorItem; first?: boolean; dragged: () => boolean }) {
  const t = useT();
  return (
    <button
      type="button"
      className="dl-ind-line"
      data-testid={first ? "ind-first" : `ind-${item.key}`}
      title={item.name}
      aria-label={t("md.openDetails", { title: item.name })}
      onClick={() => !dragged() && void api.showDownloadDetails(item.kind, item.id).catch(() => {})}
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

/** Press anywhere on the bar and move: the window moves with the pointer (a press that does not move is a click). */
function useDrag() {
  const start = useRef<{ x: number; y: number } | null>(null);
  const moved = useRef(false);
  const onPointerDown = (e: ReactPointerEvent) => {
    if (e.button !== 0 || (e.target as Element).closest("[data-no-drag]")) return;
    start.current = { x: e.clientX, y: e.clientY };
    moved.current = false;
  };
  const onPointerMove = (e: ReactPointerEvent) => {
    const s = start.current;
    if (!s || (e.buttons & 1) === 0) return void (start.current = null);
    if (Math.hypot(e.clientX - s.x, e.clientY - s.y) < DRAG_PX) return;
    start.current = null;
    moved.current = true;
    void getCurrentWindow()
      .startDragging()
      .catch(() => {});
  };
  const onPointerUp = () => void (start.current = null);
  return { handlers: { onPointerDown, onPointerMove, onPointerUp }, dragged: () => moved.current };
}

/**
 * The window takes the height of the bar: with the list open, at once (it unfolds inside the window, upwards, the
 * bar staying where it is); folded, once the list has folded. Rust keeps the bottom edge in place.
 */
function useFitWindow(card: React.RefObject<HTMLElement | null>, open: boolean, visible: boolean, hasList: boolean) {
  useLayoutEffect(() => {
    const el = card.current;
    if (!el) return;
    let last = 0;
    let later = 0;
    const measure = () => {
      const bar = el.querySelector<HTMLElement>(".dl-ind-bar");
      const list = el.querySelector<HTMLElement>(".dl-ind-scroll");
      if (!bar) return;
      const border = el.offsetHeight - el.clientHeight;
      const height = Math.ceil(border + bar.offsetHeight + (open && list ? list.offsetHeight : 0));
      if (Math.abs(height - last) < 2) return;
      const grow = height > last;
      last = height;
      window.clearTimeout(later);
      if (grow) void windowControl("fit", height);
      else later = window.setTimeout(() => void windowControl("fit", height), FOLD_MS);
    };
    const sizes = new ResizeObserver(measure);
    sizes.observe(el);
    for (const child of el.querySelectorAll(".dl-ind-bar, .dl-ind-scroll")) sizes.observe(child);
    measure();
    return () => {
      sizes.disconnect();
      window.clearTimeout(later);
    };
  }, [card, open, visible, hasList]);
}

/**
 * The download indicator's bar (its own window on the desktop, IndicatorApp): the first download (its name,
 * progress and time left) always in view; with several, an arrow shows (and hides) all the others above it.
 */
export function DownloadIndicator() {
  const t = useT();
  const active = useStore((s) => s.groups.active);
  const byId = useStore((s) => s.byId);
  const media = useStore((s) => s.media);
  const items = useMemo(() => indicatorItems(active, byId, media), [active, byId, media]);
  const [open, setOpen] = useState(false);
  const listId = useId();
  const card = useRef<HTMLElement>(null);
  const drag = useDrag();

  // When the last download ends, the bar slides away showing what it showed (Rust hides the window after that).
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
  const visible = !!first && (items.length > 0 || leaving);
  useFitWindow(card, open, visible, others.length > 0);

  if (!visible) return null;
  return (
    <section
      ref={card}
      className={`dl-ind ${leaving && items.length === 0 ? "is-leaving" : ""} ${open ? "is-open" : ""}`}
      aria-label={t("ind.label")}
      data-testid="dl-indicator"
      data-count={items.length}
      {...drag.handlers}
    >
      {others.length > 0 && (
        <div className="dl-ind-list" id={listId} data-testid="ind-list" aria-hidden={!open} inert={!open}>
          <div className="dl-ind-clip">
            <div className="dl-ind-scroll">
              {others.map((it) => (
                <Line key={it.key} item={it} dragged={drag.dragged} />
              ))}
            </div>
          </div>
        </div>
      )}
      <div className="dl-ind-bar">
        <Line item={first} first dragged={drag.dragged} />
        {others.length > 0 && (
          <button
            type="button"
            className="dl-ind-toggle"
            data-testid="ind-toggle"
            data-no-drag
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
        <button
          type="button"
          className="dl-ind-close"
          data-testid="ind-dismiss"
          data-no-drag
          aria-label={t("ind.dismiss")}
          title={t("ind.dismiss")}
          onClick={() => void windowControl("dismiss")}
        >
          <Icon name="x" size={14} />
        </button>
      </div>
    </section>
  );
}
