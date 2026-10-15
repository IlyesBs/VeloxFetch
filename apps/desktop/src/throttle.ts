// The Smart throttle state, shared by the dashboard HUD, the status bar and Settings: one copy for the whole
// window, polled every 3 s while the window is visible (it changes on its own). While it is on, <html> gets
// data-throttle (="limiting" while downloads are slowed) and the whole accent pair turns amber.
//
// Changes go through `throttleStore.save`: shown at once (the switch never waits for the answer), sent one
// after the other in the order they were made, and a poll that was already on its way when a change was made
// is dropped (it would show the old state for a moment: the switch flicking back, then forward).

import { useSyncExternalStore } from "react";
import { api, normalizeError } from "./api";
import { store } from "./hooks";
import { formatSpeed } from "./format";
import type { Key } from "./i18n";
import type { SetSmartThrottle, SmartThrottle } from "./types";

/** Manual's three levels: how much of the connection downloads keep, all the time (`keepWhileActive`). */
export const THROTTLE_MODES: {
  name: Key;
  icon: "leaf" | "scale" | "rocket";
  share: number;
}[] = [
  { name: "hud.aggressive", icon: "leaf", share: 10 },
  { name: "hud.balanced", icon: "scale", share: 25 },
  { name: "hud.performance", icon: "rocket", share: 50 },
];

/** The three ways downloads use the connection: Fast (full speed, never slowed), Smart (slowed by itself while
 * something else needs the line: a video, a game, a call, browsing) and Manual (the level the user picks, always). */
export type ThrottleLevel = "fast" | "smart" | "manual";
export const THROTTLE_LEVELS: { level: ThrottleLevel; icon: "bolt" | "activity" | "sliders" }[] = [
  { level: "fast", icon: "bolt" },
  { level: "smart", icon: "activity" },
  { level: "manual", icon: "sliders" },
];
/** What choosing a level sends. */
export const LEVEL_REQUEST: Record<ThrottleLevel, SetSmartThrottle> = {
  fast: { enabled: false },
  smart: { enabled: true, auto: true },
  manual: { enabled: true, auto: false },
};
/** The level in force (an active Download Profile may set it: Fast, or Balanced's Smart). */
export const throttleLevel = (s: SmartThrottle | null): ThrottleLevel =>
  !throttleOn(s) ? "fast" : (s?.profileAdaptive ?? s?.auto) ? "smart" : "manual";
/** Manual's level name for a share (a share set by hand elsewhere shows as a custom one). */
export const levelName = (
  t: (k: Key, v?: Record<string, string | number>) => string,
  share: number,
): string => {
  const m = THROTTLE_MODES.find((x) => x.share === share);
  return m ? t(m.name) : t("smart.modeCustom", { p: share });
};

/** What downloads keep while the user uses the internet, for a mode (mirrors `keep_while_active` in Rust). */
export const keepWhileActive = (share: number) =>
  Math.round(Math.min(0.9, Math.max(0.25, share / 100 + 0.25)) * 100);

/** Smart throttle is on (Smart or Manual): the user's choice, or the active Download Profile's (Fast holds it off). */
export const throttleOn = (s: SmartThrottle | null): boolean =>
  !!s && s.profileAdaptive !== false && (s.enabled || s.profileAdaptive === true);

/** The one-line explanation of what Smart throttle is doing now. */
export function throttleStatus(
  t: (k: Key, v?: Record<string, string | number>) => string,
  s: SmartThrottle | null,
): string {
  if (!s) return t("hud.unavailable");
  if (s.profileAdaptive === false) return t("thr.fastProfile");
  if (!throttleOn(s)) return t("thr.off");
  const speed = formatSpeed(s.limitBps ?? 0);
  switch (s.reason) {
    case "app":
      return s.gameHold
        ? t("thr.appPaused", { app: s.activeApp ?? "" })
        : t("thr.app", { app: s.activeApp ?? "", speed });
    case "browsing":
      return t("thr.browsing", { speed });
    case "busy":
      return t("thr.busy", { speed });
    case "easing":
      return t("thr.easing", { speed });
    case "fullIdle":
      return t("thr.fullIdle");
    case "unknown":
      return t("thr.unknown");
    case "checking":
      return t("thr.checking", { speed });
    case "manual":
      return s.limitBps === null
        ? t("thr.manualMeasuring", { name: levelName(t, s.sharePercent) })
        : t("thr.manual", { name: levelName(t, s.sharePercent), speed });
    default:
      return t("thr.fullFree");
  }
}

/** How much of their usual speed downloads may use now (0.04-1): the cap against the reference speed. */
export function gaugeShare(s: SmartThrottle | null): number {
  if (!throttleOn(s) || !s || s.limitBps === null) return 1;
  const ref = s.referenceBps ?? s.peakBps;
  if (!ref) {
    // No speed seen yet: the share the mode keeps (a game or a call: the mode's share; browsing: a little more).
    const keep = keepWhileActive(s.sharePercent) / 100;
    const share = s.reason === "app" ? s.sharePercent / 100 : s.reason === "busy" ? keep + (1 - keep) / 2 : keep;
    return Math.min(1, Math.max(0.04, share));
  }
  return Math.min(1, Math.max(0.04, s.limitBps / Math.max(ref, s.limitBps)));
}

let state: SmartThrottle | null = null;
/** Bumped by every change made here: a poll that started before it is stale. */
let version = 0;
/** Changes are sent one at a time, in order. */
let queue: Promise<unknown> = Promise.resolve();
/** Changes not answered yet: no poll meanwhile (it could be answered before them, with the old state). */
let saving = 0;

/** Download speed and the cap at each poll (every 3 s): the last 4 minutes, for the bandwidth graph. */
export interface ThrottlePoint {
  speed: number;
  cap: number | null;
}
const HISTORY_LEN = 80;
let history: ThrottlePoint[] = [];
const subs = new Set<() => void>();

function apply(next: SmartThrottle | null) {
  state = next;
  const root = document.documentElement;
  const mark = throttleOn(next) && next
    ? next.limitBps !== null
      ? "limiting"
      : "on"
    : undefined;
  // Only a real change touches <html>: writing the same value every poll still told the theme watchers that the
  // colours changed, and the whole window background was painted again every 3 s (it flashed).
  if (root.dataset.throttle !== mark) {
    if (mark) root.dataset.throttle = mark;
    else delete root.dataset.throttle;
  }
  subs.forEach((f) => f());
}

export const throttleStore = {
  get: () => state,
  /** A fresh state from the app (a save's answer). */
  set: (next: SmartThrottle) => {
    version++;
    apply(next);
  },
  /**
   * Change settings: shown at once, then the app's answer (the real state) replaces it. On an error the toast
   * says why and the real state comes back. Resolves with the answer (undefined on an error).
   */
  save: (patch: SetSmartThrottle, okText?: string): Promise<SmartThrottle | undefined> => {
    const mine = ++version;
    saving++;
    if (state) {
      // Off means full speed at once: no cap is shown while the answer is on its way.
      const off = patch.enabled === false ? { limitBps: null, activeApp: null, reason: "off" as const } : {};
      apply({ ...state, ...patch, apps: patch.apps ?? state.apps, ...off });
    }
    const run = queue.then(async () => {
      let failed = false;
      try {
        const next = await api.setSmartThrottle(patch);
        if (mine === version) apply(next); // a later change will bring its own answer
        if (okText) store.toast("info", okText);
        return next;
      } catch (e) {
        store.toast("error", normalizeError(e).message);
        failed = true;
        return undefined;
      } finally {
        saving--;
        if (failed) {
          version++;
          void throttleStore.refresh(); // the real state comes back
        }
      }
    });
    queue = run.catch(() => undefined);
    return run;
  },
  refresh: async () => {
    if (saving > 0) return;
    const asked = version;
    try {
      const next = await api.getSmartThrottle();
      if (asked !== version) return; // changed meanwhile: this answer is already old
      history = [
        ...history,
        {
          speed: store.getState().metrics.speedBps,
          cap: throttleOn(next) ? next.limitBps : null,
        },
      ].slice(-HISTORY_LEN);
      apply(next);
    } catch {
      /* the HUD shows "unavailable"; nothing else depends on it */
    }
  },
  subscribe(f: () => void) {
    subs.add(f);
    return () => subs.delete(f);
  },
};

export function useThrottle(): SmartThrottle | null {
  return useSyncExternalStore(throttleStore.subscribe, throttleStore.get);
}

export function useThrottleHistory(): ThrottlePoint[] {
  return useSyncExternalStore(throttleStore.subscribe, () => history);
}

/** Start polling; returns a cleanup. Called once by the app shell. */
export function watchThrottle(): () => void {
  void throttleStore.refresh();
  const t = setInterval(
    () =>
      document.visibilityState === "visible" && void throttleStore.refresh(),
    3000,
  );
  return () => clearInterval(t);
}
