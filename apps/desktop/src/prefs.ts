// Small per-user layout choices (sidebar folded, list density and order). Cosmetic only, like the theme and
// the language, so they live in localStorage under "velox.ui.*"; never anything about downloads or the user.

import { useSyncExternalStore } from "react";

export type Density = "cards" | "compact";
export type SortOrder = "status" | "newest" | "name" | "size" | "progress";

export interface UiPrefs {
  /** The user folded the sidebar to icons (it also folds by itself on narrow windows). */
  sideFolded: boolean;
  density: Density;
  sort: SortOrder;
}

const KEY = "velox.ui";
const DEFAULTS: UiPrefs = { sideFolded: false, density: "cards", sort: "status" };
const SORTS: SortOrder[] = ["status", "newest", "name", "size", "progress"];

function load(): UiPrefs {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? "{}") as Partial<UiPrefs>;
    return {
      sideFolded: v.sideFolded === true,
      density: v.density === "compact" ? "compact" : "cards",
      sort: SORTS.includes(v.sort as SortOrder) ? (v.sort as SortOrder) : "status",
    };
  } catch {
    return DEFAULTS;
  }
}

let current = load();
const subs = new Set<() => void>();

export function setPrefs(patch: Partial<UiPrefs>): void {
  current = { ...current, ...patch };
  try {
    localStorage.setItem(KEY, JSON.stringify(current));
  } catch {
    /* storage may be unavailable */
  }
  subs.forEach((f) => f());
}

function subscribe(f: () => void) {
  subs.add(f);
  return () => {
    subs.delete(f);
  };
}

export function usePrefs(): UiPrefs {
  return useSyncExternalStore(subscribe, () => current);
}

/** Whether a media query matches, kept up to date (window width breakpoints). */
export function useMedia(query: string): boolean {
  return useSyncExternalStore(
    (f) => {
      const mq = matchMedia(query);
      mq.addEventListener("change", f);
      return () => mq.removeEventListener("change", f);
    },
    () => matchMedia(query).matches,
  );
}
