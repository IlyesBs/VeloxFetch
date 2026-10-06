import { useRef, type PointerEvent } from "react";
import { useStore } from "../../hooks";
import { useT, type Key } from "../../i18n";
import { setPrefs } from "../../prefs";
import { Icon, Logo, type IconName } from "../../ui/icons";
import { ThrottleHud } from "../ThrottleHud";

export type RouteName = "downloads" | "active" | "queue" | "completed" | "history" | "settings";
/** What the sidebar's rows lead to: each row has its own view. */
export type NavTarget = RouteName;

const TABS: { name: NavTarget; label: Key; icon: IconName; keys?: string }[] = [
  { name: "downloads", label: "nav.downloads", icon: "inbox", keys: "Ctrl+1" },
  { name: "active", label: "nav.active", icon: "activeBox" },
  { name: "queue", label: "nav.queue", icon: "layers", keys: "Ctrl+2" },
  { name: "completed", label: "nav.completed", icon: "checkCircle" },
  { name: "history", label: "nav.history", icon: "history", keys: "Ctrl+3" },
];

/**
 * One navigation row. Its icon leans toward the pointer ("magnetic") and springs back when it leaves; the
 * transform is set through the style object (CSSOM), which the strict CSP allows. Folded, only the icon
 * shows and the label becomes a tooltip (it stays the button's accessible name).
 */
function NavRow({
  label,
  icon,
  current,
  badge,
  testId,
  keys,
  onClick,
}: {
  label: string;
  icon: IconName;
  current: boolean;
  badge?: number;
  testId: string;
  keys?: string;
  onClick: () => void;
}) {
  const iconRef = useRef<HTMLSpanElement>(null);
  const move = (e: PointerEvent<HTMLButtonElement>) => {
    const r = iconRef.current?.getBoundingClientRect();
    if (!r || !iconRef.current) return;
    const dx = (e.clientX - (r.left + r.width / 2)) * 0.22;
    const dy = (e.clientY - (r.top + r.height / 2)) * 0.22;
    iconRef.current.style.transform = `translate(${Math.max(-4, Math.min(4, dx)).toFixed(1)}px, ${Math.max(-4, Math.min(4, dy)).toFixed(1)}px) scale(1.08)`;
  };
  const leave = () => {
    if (iconRef.current) iconRef.current.style.transform = "";
  };
  return (
    <button
      className="side-link"
      aria-label={label}
      aria-current={current ? "page" : undefined}
      aria-keyshortcuts={keys}
      data-testid={testId}
      onClick={onClick}
      onPointerMove={move}
      onPointerLeave={leave}
    >
      <span ref={iconRef} className="side-icon">
        <Icon name={icon} size={21} />
      </span>
      <span className="side-label truncate">{label}</span>
      {!!badge && <span className="side-badge">{badge > 99 ? "99+" : badge}</span>}
      <span className="side-tip" aria-hidden="true">
        {label}
        {keys && <span className="ms-2 text-dim">{keys}</span>}
      </span>
    </button>
  );
}

/**
 * The sidebar: the brand, the navigation and (unfolded) the Smart throttle card. It folds to an icon rail
 * on narrow windows or when the user folds it; the Smart throttle then moves into the page (App).
 */
export function Sidebar({ current, folded, canUnfold, onNav }: { current: NavTarget | "detail"; folded: boolean; canUnfold: boolean; onNav: (t: NavTarget) => void }) {
  const t = useT();
  const active = useStore((s) => s.metrics.active);
  const queued = useStore((s) => s.metrics.queued);
  const badge = (name: NavTarget) => (name === "active" ? active : name === "queue" ? queued : 0);
  // the detail view belongs to "Downloads"
  const lit = current === "detail" ? "downloads" : current;

  return (
    <aside className={`shell-side ${folded ? "" : "overflow-y-auto overflow-x-hidden"}`} data-folded={folded} data-testid="rail">
      <div className="side-brand" data-tauri-drag-region>
        <Logo size={folded ? 34 : 40} />
      </div>
      <nav aria-label={t("nav.main")} className={`flex flex-col gap-1.5 ${folded ? "items-center px-0" : "px-3"}`}>
        {TABS.map((tab) => (
          <NavRow
            key={tab.name}
            label={t(tab.label)}
            icon={tab.icon}
            keys={tab.keys}
            current={lit === tab.name}
            badge={badge(tab.name)}
            testId={`nav-${tab.name}`}
            onClick={() => onNav(tab.name)}
          />
        ))}
      </nav>
      <div className={`mt-1.5 ${folded ? "flex justify-center" : "px-3"}`}>
        <NavRow label={t("nav.settings")} icon="gear" keys="Ctrl+," current={lit === "settings"} testId="nav-settings" onClick={() => onNav("settings")} />
      </div>

      {!folded && (
        <div className="mt-5 border-t border-line px-3 pt-5">
          <ThrottleHud variant="side" />
        </div>
      )}

      <div className="flex-1" />
      {canUnfold && (
        <div className="flex justify-center py-3">
          <button
            className="tool-btn h-9 w-9"
            aria-label={folded ? t("nav.expand") : t("nav.collapse")}
            title={folded ? t("nav.expand") : t("nav.collapse")}
            data-testid="side-fold"
            onClick={() => setPrefs({ sideFolded: !folded })}
          >
            <Icon name={folded ? "unfold" : "fold"} size={17} className="flip-rtl" />
          </button>
        </div>
      )}
    </aside>
  );
}
