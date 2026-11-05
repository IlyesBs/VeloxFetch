import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Icon, type IconName } from "./icons";
import { store, useStore } from "../hooks";
import type { DownloadStatus } from "../types";
import { useT } from "../i18n";

// ------------------------------------------------------------------ progress bar

const TONES = { accent: "bar-accent", ok: "bar-ok", warn: "bg-warn", bad: "bg-bad", dim: "bg-dim" } as const;

/**
 * `value` is 0..1. Animated with transform only (GPU-composited), never by changing layout. The accent
 * tone is the accent-pair gradient with a soft glow; `live` adds a light sweeping over a running bar.
 */
export function ProgressBar({
  value,
  tone = "accent",
  label,
  className = "",
  live = false,
  thick = false,
}: {
  value: number;
  tone?: keyof typeof TONES;
  label: string;
  className?: string;
  live?: boolean;
  thick?: boolean;
}) {
  const v = Math.min(1, Math.max(0, value));
  return (
    <div
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(v * 100)}
      className={`${thick ? "h-2" : "h-1.5"} w-full overflow-hidden rounded-full bg-line ${className}`}
    >
      <div className={`bar-fill h-full w-full rounded-full ${TONES[tone]} ${live ? "bar-live" : ""}`} style={{ transform: `scaleX(${v})` }} />
    </div>
  );
}

// ------------------------------------------------------------------ sparkline

/**
 * Area chart of a series, scaled to its own max so the shape is always readable: a glowing line in the
 * accent over a gradient that fades into the card. Colours come from CSS classes (theme + throttle aware).
 */
export function Sparkline({ values, className = "", label }: { values: number[]; className?: string; label: string }) {
  const id = useId().replace(/:/g, "");
  const W = 100;
  const H = 28;
  const n = values.length;
  const max = Math.max(1, ...values);
  const pts = values.map((v, i) => [n <= 1 ? W : (i / (n - 1)) * W, H - 1 - (v / max) * (H - 3)] as const);
  const line = pts.map(([x, y], i) => `${i ? "L" : "M"}${x.toFixed(2)} ${y.toFixed(2)}`).join(" ");
  const area = n > 1 ? `${line} L${W} ${H} L0 ${H} Z` : "";
  return (
    <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" role="img" aria-label={label} className={`spark block w-full ${className}`}>
      <defs>
        <linearGradient id={`sf${id}`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" className="stop-accent" stopOpacity={0.38} />
          <stop offset="1" className="stop-accent-2" stopOpacity={0} />
        </linearGradient>
        <linearGradient id={`sl${id}`} x1="0" y1="0" x2="1" y2="0">
          <stop offset="0" className="stop-accent-2" />
          <stop offset="1" className="stop-accent" />
        </linearGradient>
      </defs>
      {area && <path d={area} fill={`url(#sf${id})`} />}
      {n > 1 && <path className="spark-line" d={line} fill="none" stroke={`url(#sl${id})`} strokeWidth={2} vectorEffect="non-scaling-stroke" strokeLinejoin="round" strokeLinecap="round" />}
    </svg>
  );
}

// ------------------------------------------------------------------ status chip

const CHIP: Record<DownloadStatus, string> = {
  downloading: "bg-accent-soft text-accent",
  verifying: "bg-accent-soft text-accent",
  queued: "bg-line text-dim",
  paused: "bg-warn/15 text-warn",
  completed: "bg-ok/15 text-ok",
  failed: "bg-bad/15 text-bad",
  cancelled: "bg-line text-dim",
};

export function StatusChip({ status }: { status: DownloadStatus }) {
  const t = useT();
  return (
    <span className={`chip ${CHIP[status]}`}>
      {(status === "downloading" || status === "verifying") && <span className="h-1.5 w-1.5 rounded-full bg-current" />}
      {t(`status.${status}`)}
    </span>
  );
}

// ------------------------------------------------------------------ dropdown menu

export interface MenuItem {
  label: string;
  icon?: IconName;
  onSelect: () => void;
  danger?: boolean;
  disabled?: boolean;
}

function MenuList({ id, items, onDone, className, style }: { id?: string; items: MenuItem[]; onDone: () => void; className: string; style?: CSSProperties }) {
  return (
    <div id={id} role="menu" className={`card rise min-w-48 p-1 shadow-lg ${className}`} style={style}>
      {items.map((it) => (
        <button
          key={it.label}
          type="button"
          role="menuitem"
          disabled={it.disabled}
          className={`flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-start text-[13px] hover:bg-accent-soft disabled:opacity-40 ${it.danger ? "text-bad" : ""}`}
          onClick={(e) => {
            e.stopPropagation();
            onDone();
            it.onSelect();
          }}
        >
          {it.icon && <Icon name={it.icon} size={15} />}
          {it.label}
        </button>
      ))}
    </div>
  );
}

/** Where a menu opens: at the pointer, or under (or, if there is no room, above) a button. */
export type MenuAnchor = { x: number; y: number } | { right: number; top: number; bottom: number };

/**
 * A menu drawn on the page itself (a portal on <body>, position: fixed), never inside a card: cards
 * have their own stacking context, so a menu that lived inside one was painted UNDER the card below
 * it. Always kept inside the window. Closed by click-away, Escape, scroll, resize or losing focus.
 * `ignore` is the button that opened it (a click on it toggles instead of closing and reopening).
 */
export function ContextMenu({ at, items, onClose, ignore }: { at: MenuAnchor; items: MenuItem[]; onClose: () => void; ignore?: React.RefObject<HTMLElement | null> }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ x: number; y: number } | null>(null);
  useLayoutEffect(() => {
    const r = ref.current?.getBoundingClientRect();
    if (!r) return;
    const clampX = (x: number) => Math.max(4, Math.min(x, window.innerWidth - r.width - 4));
    const clampY = (y: number) => Math.max(4, Math.min(y, window.innerHeight - r.height - 4));
    if ("right" in at) {
      const below = at.bottom + 4;
      const y = below + r.height <= window.innerHeight - 4 ? below : at.top - 4 - r.height;
      setPos({ x: clampX(at.right - r.width), y: clampY(y) });
    } else {
      setPos({ x: clampX(at.x), y: clampY(at.y) });
    }
  }, [at]);
  useEffect(() => {
    const away = (e: Event) => {
      const t = e.target as Node;
      if (!ref.current?.contains(t) && !ignore?.current?.contains(t)) onClose();
    };
    const esc = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("pointerdown", away, true);
    document.addEventListener("keydown", esc);
    window.addEventListener("blur", onClose);
    window.addEventListener("resize", onClose);
    document.addEventListener("wheel", onClose, { passive: true });
    return () => {
      document.removeEventListener("pointerdown", away, true);
      document.removeEventListener("keydown", esc);
      window.removeEventListener("blur", onClose);
      window.removeEventListener("resize", onClose);
      document.removeEventListener("wheel", onClose);
    };
  }, [onClose, ignore]);
  return createPortal(
    <div
      ref={ref}
      className="fixed z-50"
      style={{ left: pos?.x ?? 0, top: pos?.y ?? 0, opacity: pos ? 1 : 0 }}
      // React events cross portals: keep clicks in the menu from reaching the card underneath.
      onClick={(e) => e.stopPropagation()}
      onContextMenu={(e) => {
        e.preventDefault();
        e.stopPropagation();
      }}
    >
      <MenuList items={items} onDone={onClose} className="" />
    </div>,
    document.body,
  );
}

/** The "..." button and its menu. */
export function Menu({ label, items, vertical = false }: { label: string; items: MenuItem[]; vertical?: boolean }) {
  const [anchor, setAnchor] = useState<MenuAnchor | null>(null);
  const button = useRef<HTMLButtonElement>(null);
  const close = useCallback(() => setAnchor(null), []);
  return (
    <>
      <button
        ref={button}
        type="button"
        className={vertical ? "round-btn round-ghost" : "btn btn-ghost btn-icon"}
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={anchor !== null}
        onClick={(e) => {
          e.stopPropagation();
          if (anchor) return setAnchor(null);
          const r = e.currentTarget.getBoundingClientRect();
          setAnchor({ right: r.right, top: r.top, bottom: r.bottom });
        }}
      >
        <Icon name={vertical ? "moreV" : "more"} size={vertical ? 20 : 16} />
      </button>
      {anchor && <ContextMenu at={anchor} items={items} onClose={close} ignore={button} />}
    </>
  );
}

// ------------------------------------------------------------------ toasts

export function Toasts() {
  const tr = useT();
  const toasts = useStore((s) => s.toasts);
  useEffect(() => {
    if (toasts.length === 0) return;
    const timers = toasts.map((t) => setTimeout(() => store.dismissToast(t.id), t.kind === "error" ? 8000 : 3500));
    return () => timers.forEach(clearTimeout);
  }, [toasts]);
  return (
    <div className="pointer-events-none fixed bottom-10 end-4 z-50 flex w-80 flex-col gap-2" aria-live="polite">
      {toasts.map((t) => (
        <div
          key={t.id}
          role={t.kind === "error" ? "alert" : "status"}
          className={`card rise pointer-events-auto flex items-start gap-2 p-3 shadow-lg ${t.kind === "error" ? "border-bad/40" : ""}`}
        >
          <Icon name={t.kind === "error" ? "alert" : "check"} className={t.kind === "error" ? "text-bad" : "text-ok"} />
          <p className="flex-1 break-words text-[13px]">{t.text}</p>
          <button className="text-dim hover:text-fg" aria-label={tr("toast.dismiss")} data-testid="toast-dismiss" onClick={() => store.dismissToast(t.id)}>
            <Icon name="x" size={14} />
          </button>
        </div>
      ))}
    </div>
  );
}

// ------------------------------------------------------------------ misc

/** A titled settings card. */
export function Panel({ id, title, hint, children }: { id?: string; title: string; hint: string; children: ReactNode }) {
  return (
    <section id={id} className="card scroll-mt-4 p-5">
      <h2 className="text-[14px] font-semibold">{title}</h2>
      <p className="mb-4 mt-0.5 text-xs text-dim">{hint}</p>
      {children}
    </section>
  );
}

export function Section({ title, hint, children, right }: { title: string; hint?: string; right?: ReactNode; children: ReactNode }) {
  return (
    <section className="space-y-2.5">
      <div className="flex items-baseline justify-between">
        <h2 className="display text-[15px] font-semibold">
          {title}
          {hint && <span className="num ms-2 rounded-full bg-accent-soft px-2 py-0.5 align-middle text-[11px] font-semibold text-accent">{hint}</span>}
        </h2>
        {right}
      </div>
      {children}
    </section>
  );
}

export function EmptyState({ title, body, icon }: { title: string; body: string; icon: IconName }) {
  return (
    <div className="card flex flex-col items-center gap-2 px-6 py-12 text-center">
      <div className="logo-mark grid h-14 w-14 place-items-center rounded-2xl bg-accent-soft text-accent">
        <Icon name={icon} size={26} />
      </div>
      <p className="font-medium">{title}</p>
      <p className="max-w-sm text-dim">{body}</p>
    </div>
  );
}
