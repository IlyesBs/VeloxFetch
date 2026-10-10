// Theme preference: "system" follows the OS, "light"/"dark" force it. Stored only in localStorage
// (a per-user cosmetic setting, so no need for the Rust side). Applied through a data attribute on
// <html>, which the CSS tokens key off. No inline styles are involved (strict CSP).

import { syncFrameTheme } from "./chrome";

export type ThemePref = "system" | "light" | "dark";
const KEY = "velox.theme";

export function getThemePref(): ThemePref {
  try {
    const v = localStorage.getItem(KEY);
    if (v === "light" || v === "dark" || v === "system") return v;
  } catch {
    /* storage may be unavailable */
  }
  return "system";
}

export function applyTheme(pref: ThemePref): void {
  const root = document.documentElement;
  if (pref === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", pref);
  // the native frame (Mica tint, macOS title bar) follows the app's theme too
  syncFrameTheme(pref);
  themeSubs.forEach((f) => f());
}

const themeSubs = new Set<() => void>();
/** Re-render when the theme preference changes (title bar toggle and Settings stay in step). */
export function subscribeThemePref(f: () => void): () => void {
  themeSubs.add(f);
  return () => {
    themeSubs.delete(f);
  };
}

export function setThemePref(pref: ThemePref): void {
  try {
    localStorage.setItem(KEY, pref);
  } catch {
    /* ignore */
  }
  applyTheme(pref);
}

/** Resolved colours for canvas drawing (canvas can't use CSS variables directly). */
export interface Palette {
  track: string;
  accent: string;
  accent2: string;
  ok: string;
  warn: string;
  bad: string;
  dim: string;
  fg: string;
}

export function readPalette(): Palette {
  const cs = getComputedStyle(document.documentElement);
  const v = (name: string, fallback: string) => cs.getPropertyValue(name).trim() || fallback;
  return {
    track: v("--line-strong", "#cbd5e1"),
    accent: v("--accent", "#3a86ff"),
    accent2: v("--accent-2", "#0a2540"),
    ok: v("--ok", "#16a34a"),
    warn: v("--warn", "#d97706"),
    bad: v("--bad", "#dc2626"),
    dim: v("--dim", "#64748b"),
    fg: v("--fg", "#0f172a"),
  };
}

/**
 * What the colours depend on: the theme, whether Smart throttle is on (any value: "on" and "limiting" share one
 * palette) and the system's dark mode.
 */
export function paletteKey(theme: string | null, throttle: string | null, systemDark: boolean): string {
  return `${theme ?? "system"}|${throttle !== null}|${theme ? "" : systemDark}`;
}

/** Call `cb` whenever the colours really changed (theme, Smart throttle on or off, the system's dark mode). */
export function onThemeChange(cb: () => void): () => void {
  const root = document.documentElement;
  const mq = matchMedia("(prefers-color-scheme: dark)");
  const key = () => paletteKey(root.getAttribute("data-theme"), root.getAttribute("data-throttle"), mq.matches);
  let last = key();
  // Repaints (the window background is a full-size picture) only when the palette changes: Smart throttle slowing
  // downloads or not, or an attribute written again with the same value, keeps the same colours.
  const changed = () => {
    const now = key();
    if (now === last) return;
    last = now;
    cb();
  };
  const mo = new MutationObserver(changed);
  mo.observe(root, { attributes: true, attributeFilter: ["data-theme", "data-throttle"] });
  mq.addEventListener("change", changed);
  return () => {
    mo.disconnect();
    mq.removeEventListener("change", changed);
  };
}
