// The Smart throttle state, shared by the dashboard HUD, the status bar and Settings. Polled every 3 s
// while the window is visible (it changes on its own). While it is on, <html> gets data-throttle
// (="limiting" while downloads are slowed) and the whole accent pair turns amber.

import { useSyncExternalStore } from "react";
import { api } from "./api";
import { store } from "./hooks";
import { formatSpeed } from "./format";
import type { Key } from "./i18n";
import type { SmartThrottle } from "./types";

/** The three modes: the share downloads keep during a game or a call (and, a little more, while browsing). */
export const THROTTLE_MODES: {
  name: Key;
  icon: "leaf" | "scale" | "rocket";
  share: number;
}[] = [
  { name: "hud.aggressive", icon: "leaf", share: 10 },
  { name: "hud.balanced", icon: "scale", share: 25 },
  { name: "hud.conservative", icon: "rocket", share: 50 },
];

/** What downloads keep while the user uses the internet, for a mode (mirrors `keep_while_active` in Rust). */
export const keepWhileActive = (share: number) =>
  Math.round(Math.min(0.9, Math.max(0.25, share / 100 + 0.25)) * 100);

/** The one-line explanation of what Smart throttle is doing now. */
export function throttleStatus(
  t: (k: Key, v?: Record<string, string | number>) => string,
  s: SmartThrottle | null,
): string {
  if (!s) return t("hud.unavailable");
  if (!s.enabled) return t("thr.off");
  const speed = formatSpeed(s.limitBps ?? 0);
  switch (s.reason) {
    case "app":
      return t("thr.app", { app: s.activeApp ?? "", speed });
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
    default:
      return t("thr.fullFree");
  }
}

let state: SmartThrottle | null = null;

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
  if (next?.enabled)
    root.dataset.throttle = next.limitBps !== null ? "limiting" : "on";
  else delete root.dataset.throttle;
  subs.forEach((f) => f());
}

export const throttleStore = {
  get: () => state,
  /** A fresh state from a save (Settings, the HUD). */
  set: (next: SmartThrottle) => apply(next),
  refresh: async () => {
    try {
      const next = await api.getSmartThrottle();
      history = [
        ...history,
        {
          speed: store.getState().metrics.speedBps,
          cap: next.enabled ? next.limitBps : null,
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
