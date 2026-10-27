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
  /** The queue tab last open on the Queue page. */
  queue: number;
}

const KEY = "velox.ui";
const DEFAULTS: UiPrefs = { sideFolded: false, density: "cards", sort: "status", queue: 1 };
const SORTS: SortOrder[] = ["status", "newest", "name", "size", "progress"];

function load(): UiPrefs {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? "{}") as Partial<UiPrefs>;
    return {
      sideFolded: v.sideFolded === true,
      density: v.density === "compact" ? "compact" : "cards",
      sort: SORTS.includes(v.sort as SortOrder) ? (v.sort as SortOrder) : "status",
      queue: Number.isInteger(v.queue) && (v.queue as number) > 0 ? (v.queue as number) : 1,
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

/**
 * Whether a media query matches, kept up to date (window width breakpoints). `whenUnsized`: the answer while the page
 * has no size yet. On Linux the window is created hidden and its page is 0 x 0 until it is shown: a width query then
 * says "narrow" and the first render drew the narrow layout (folded sidebar), which jumped to the real one when the
 * window appeared ("the UI flashes when I open the app"). The window opens at its full size, so the caller says what
 * to assume; the real answer follows as soon as the page has a size (a resize, even one that does not change the
 * query's answer).
 */
export function useMedia(query: string, whenUnsized?: boolean): boolean {
  return useSyncExternalStore(
    (f) => {
      const mq = matchMedia(query);
      mq.addEventListener("change", f);
      addEventListener("resize", f);
      return () => {
        mq.removeEventListener("change", f);
        removeEventListener("resize", f);
      };
    },
    () => (whenUnsized !== undefined && innerWidth === 0 ? whenUnsized : matchMedia(query).matches),
  );
}
