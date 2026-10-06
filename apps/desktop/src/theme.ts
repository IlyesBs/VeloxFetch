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

/** Call `cb` whenever the effective theme may have changed (attribute flip or OS preference). */
export function onThemeChange(cb: () => void): () => void {
  const mo = new MutationObserver(cb);
  mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme", "data-throttle"] });
  const mq = matchMedia("(prefers-color-scheme: dark)");
  mq.addEventListener("change", cb);
  return () => {
    mo.disconnect();
    mq.removeEventListener("change", cb);
  };
}
