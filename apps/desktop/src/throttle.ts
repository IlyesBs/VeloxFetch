// The Smart throttle state, shared by the dashboard HUD, the status bar and Settings. Polled every 3 s
// while the window is visible (which programs run changes on its own). While it is on, <html> gets
// data-throttle (="limiting" while a listed program runs) and the whole accent pair turns amber.

import { useSyncExternalStore } from "react";
import { api } from "./api";
import type { SmartThrottle } from "./types";

let state: SmartThrottle | null = null;
const subs = new Set<() => void>();

function apply(next: SmartThrottle | null) {
  state = next;
  const root = document.documentElement;
  if (next?.enabled) root.dataset.throttle = next.activeApp ? "limiting" : "on";
  else delete root.dataset.throttle;
  subs.forEach((f) => f());
}

export const throttleStore = {
  get: () => state,
  /** A fresh state from a save (Settings, the HUD). */
  set: (next: SmartThrottle) => apply(next),
  refresh: async () => {
    try {
      apply(await api.getSmartThrottle());
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

/** Start polling; returns a cleanup. Called once by the app shell. */
export function watchThrottle(): () => void {
  void throttleStore.refresh();
  const t = setInterval(() => document.visibilityState === "visible" && void throttleStore.refresh(), 3000);
  return () => clearInterval(t);
}
