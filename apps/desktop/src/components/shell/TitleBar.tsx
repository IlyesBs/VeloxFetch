import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { ownWindowButtons, windowControl } from "../../chrome";
import { LANGS, setLang, useLang, useT, type Key } from "../../i18n";
import { getThemePref, setThemePref, subscribeThemePref, type ThemePref } from "../../theme";
import { Icon, type IconName } from "../../ui/icons";
import type { RouteName } from "./Sidebar";

const PAGE: Record<RouteName | "detail", Key> = {
  downloads: "nav.downloads",
  active: "nav.active",
  queue: "nav.queue",
  completed: "nav.completed",
  history: "nav.history",
  settings: "nav.settings",
  detail: "nav.detail",
};

const NEXT_THEME: Record<ThemePref, ThemePref> = { system: "light", light: "dark", dark: "system" };
const THEME_ICON: Record<ThemePref, IconName> = { system: "gauge", light: "sun", dark: "moon" };

/** Window buttons drawn by the UI (Windows / Linux; macOS keeps its native traffic lights). */
function WindowButtons() {
  const t = useT();
  const [maximized, setMaximized] = useState(false);
  useEffect(() => {
    // the window's size changes (snap, double-click on the bar, a keyboard shortcut): refresh the icon
    const sync = () => void windowControl("isMaximized").then((m) => m !== null && setMaximized(m));
    sync();
    window.addEventListener("resize", sync);
    return () => window.removeEventListener("resize", sync);
  }, []);
  const run = (action: "minimize" | "toggleMaximize" | "close") => void windowControl(action).then((m) => m !== null && setMaximized(m));
  // 10 px glyphs, 1 px strokes: the look of the system's own caption buttons.
  const glyph = (d: string) => (
    <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1" shapeRendering="crispEdges">
      <path d={d} />
    </svg>
  );
  return (
    <div className="winbtns flex h-full items-center" data-testid="window-buttons">
      <button className="winbtn" aria-label={t("win.minimize")} title={t("win.minimize")} onClick={() => run("minimize")}>
        {glyph("M0 5.5h10")}
      </button>
      <button className="winbtn" aria-label={maximized ? t("win.restore") : t("win.maximize")} title={maximized ? t("win.restore") : t("win.maximize")} onClick={() => run("toggleMaximize")}>
        {maximized ? glyph("M2.5 2.5V.5h7v7h-2M.5 2.5h7v7h-7z") : glyph("M.5.5h9v9h-9z")}
      </button>
      <button className="winbtn winbtn-close" aria-label={t("win.close")} title={t("win.close")} onClick={() => run("close")}>
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.1">
          <path d="M.5.5l9 9M9.5.5l-9 9" />
        </svg>
      </button>
    </div>
  );
}

/**
 * The language picker: a globe chip that opens the list (Arabic mirrors the interface right-to-left).
 * The list stays in the page while closed (hidden), so the choices are always reachable by their ids.
 */
function LanguageSwitch() {
  const t = useT();
  const lang = useLang();
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: Event) => !box.current?.contains(e.target as Node) && setOpen(false);
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    const blur = () => setOpen(false);
    document.addEventListener("pointerdown", away, true);
    document.addEventListener("keydown", esc);
    window.addEventListener("blur", blur);
    return () => {
      document.removeEventListener("pointerdown", away, true);
      document.removeEventListener("keydown", esc);
      window.removeEventListener("blur", blur);
    };
  }, [open]);
  const cur = LANGS.find((l) => l.lang === lang) ?? LANGS[0]!;
  return (
    <div ref={box} className="relative" data-testid="lang-switch">
      <button className="chip-btn" aria-haspopup="true" aria-expanded={open} aria-label={`${t("win.language")}: ${cur.label}`} title={t("win.language")} data-testid="lang-open" onClick={() => setOpen(!open)}>
        <Icon name="globe" size={18} />
        <span className="hidden min-w-5 sm:inline">{cur.short}</span>
        <Icon name="chevronDown" size={14} className="text-dim" />
      </button>
      <div role="radiogroup" aria-label={t("win.language")} className="popover end-0 top-12 max-h-[min(70vh,26rem)] overflow-y-auto" hidden={!open}>
        {LANGS.map((l) => (
          <button
            key={l.lang}
            role="radio"
            aria-checked={lang === l.lang}
            data-testid={`lang-${l.lang}`}
            className="popover-item"
            onClick={() => {
              setLang(l.lang);
              setOpen(false);
            }}
          >
            <span className="w-6 text-[11px] font-bold text-dim">{l.short}</span>
            <span className="flex-1">{l.label}</span>
            {lang === l.lang && <Icon name="check" size={14} />}
          </button>
        ))}
      </div>
    </div>
  );
}

function ThemeToggle() {
  const t = useT();
  const pref = useSyncExternalStore(subscribeThemePref, getThemePref);
  const name = t(`theme.${pref}`);
  return (
    <button className="round-btn" aria-label={t("win.theme", { name })} title={t("win.theme", { name })} data-testid="theme-toggle" onClick={() => setThemePref(NEXT_THEME[pref])}>
      <Icon name={THEME_ICON[pref]} size={19} />
    </button>
  );
}

/**
 * The frameless window's title bar (beside the sidebar). Empty space drags the window (and double-click
 * maximizes); `data-tauri-drag-region` must be on each element that should drag, not only on the parent.
 */
export function TitleBar({ route }: { route: RouteName | "detail" }) {
  const t = useT();
  return (
    <header className="titlebar relative z-30 flex shrink-0 items-center gap-3 ps-6" data-tauri-drag-region data-testid="titlebar">
      <div className="flex min-w-0 items-center gap-2.5" data-tauri-drag-region>
        <span className="brand-name truncate text-[19px]" data-tauri-drag-region>
          VeloxFetch
        </span>
        <span className="version-pill num" title={t("app.version", { v: __APP_VERSION__ })} data-testid="app-version" data-tauri-drag-region>
          v{__APP_VERSION__}
        </span>
        {/* the page's name, for screen readers and the window's accessibility tree (the sidebar shows it) */}
        <h1 className="sr-only" data-testid="page-title">
          {t(PAGE[route])}
        </h1>
      </div>
      <div className="h-full min-w-4 flex-1" data-tauri-drag-region />
      <div className="flex items-center gap-2.5 pe-2">
        <LanguageSwitch />
        <ThemeToggle />
      </div>
      {ownWindowButtons() && <WindowButtons />}
    </header>
  );
}
