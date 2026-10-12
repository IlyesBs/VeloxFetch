import { useCallback, useEffect, useState, type AnimationEvent } from "react";

/**
 * The entrance animation (`.rise`: fade in from 0) plays once per item, the first time it is drawn in this window.
 *
 * Why: a browser restarts an element's CSS animation whenever the element is put back into the page. React does
 * that when a list changes order (a download going from "waiting" to "running" moves every card it passes) and
 * when a card changes section (Active: waiting / running / paused). With `.rise` still on, every one of those
 * cards faded out to nothing and back in: the whole list "flashed" when a download started. So the class is
 * dropped as soon as the animation has played, and an item already seen never gets it again.
 */
const seen = new Set<string>();

/** Has `key` already been drawn in this window? */
export const wasShown = (key: string): boolean => seen.has(key);
/** Mark `key` as drawn (its entrance never plays again). */
export const markShown = (key: string): void => void seen.add(key);
/** Tests only. */
export const forgetShown = (): void => seen.clear();

/** Longest the `.rise` animation can take (styles.css: 220 ms), with room for a slow first frame. */
export const ENTRANCE_MS = 600;

/**
 * `[className, onAnimationEnd]` for an item's root element: `"rise"` on its first appearance until the animation
 * ends (or `ENTRANCE_MS` passed, if the element was never painted), `""` afterwards and on every later mount.
 */
export function useEntrance(key: string): [string, ((e: AnimationEvent) => void) | undefined] {
  // Read only here (StrictMode calls this twice); marked in the effect below.
  const [entering, setEntering] = useState(() => !wasShown(key));
  useEffect(() => {
    markShown(key);
    if (!entering) return;
    const t = setTimeout(() => setEntering(false), ENTRANCE_MS);
    return () => clearTimeout(t);
  }, [key, entering]);
  const end = useCallback((e: AnimationEvent) => {
    if (e.target === e.currentTarget && e.animationName === "rise") setEntering(false);
  }, []);
  return entering ? ["rise", end] : ["", undefined];
}
