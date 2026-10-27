import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { api, normalizeError, onManagerEvent } from "./api";
import { createStore, type State } from "./store";
import type { Download, DownloadDetail } from "./types";

/** The single app store (see store.ts for the rendering model). */
export const store = createStore();

export function useStore<T>(selector: (s: State) => T): T {
  return useSyncExternalStore(store.subscribe, () => selector(store.getState()));
}

export function useDownload(id: number): Download | undefined {
  return useStore((s) => s.byId[id]);
}

const IDLE_REFRESH_MS = 5000;
const FOCUS_RESYNC_MS = 3000;
/** When the last engine tick arrived (ms). Ticks stop while nothing downloads. */
let lastTick = 0;
export const lastTickAt = () => lastTick;

/**
 * While mounted (the dashboard's side panels), keep the network / disk numbers live even with no download
 * running: ask for the metrics once a second whenever the engine's ticks have stopped.
 */
export function useLiveIo() {
  useEffect(() => {
    const t = setInterval(() => {
      if (document.visibilityState !== "visible" || Date.now() - lastTick < 1200) return;
      api.getMetrics().then(
        (m) => Date.now() - lastTick >= 1200 && store.setMetrics(m),
        () => {},
      );
    }, 1000);
    return () => clearInterval(t);
  }, []);
}

/** Load the initial snapshot and keep the store fed with engine events. Returns a cleanup. */
export async function bootstrap(): Promise<() => void> {
  const unlisten = await onManagerEvent((ev) => {
    if (ev.type === "tick") lastTick = Date.now();
    store.dispatch(ev);
  });
  const reload = async () => {
    try {
      const [downloads, metrics, queue, pending, media, queues] = await Promise.all([
        api.listDownloads(),
        api.getMetrics(),
        api.getQueueState().catch(() => null), // the queue panel degrades on its own; the list must not
        api.listPendingDownloads().catch(() => undefined),
        api.listMediaJobs().catch(() => undefined),
        api.getQueues().catch(() => null),
      ]);
      store.load(downloads, metrics, queue, pending, media, queues);
      // Download Profiles: the panel and the status bar degrade on their own if this fails.
      api.getProfiles().then(
        (profiles) => store.dispatch({ type: "profiles", profiles }),
        () => {},
      );
      // Updates: Settings > Updates and the status bar degrade on their own too.
      api.getUpdateStatus().then(
        (update) => store.dispatch({ type: "update", update }),
        () => {},
      );
      // The required dependencies (an update offered): the notice degrades on its own too.
      api.getVideoTools().then(
        (tools) => store.dispatch({ type: "videoTools", tools }),
        () => {},
      );
    } catch (e) {
      store.toast("error", normalizeError(e).message);
    }
  };
  await reload();
  // Events are best-effort (a slow UI can miss some), so resync whenever the window comes back.
  const onVisible = () => document.visibilityState === "visible" && void reload();
  document.addEventListener("visibilitychange", onVisible);
  // Coming back from another program (a file manager where a finished file was deleted): resync too, at most
  // every few seconds, so "Download again" shows for a file that is gone.
  let lastFocus = 0;
  const onFocus = () => {
    if (Date.now() - lastFocus < FOCUS_RESYNC_MS) return;
    lastFocus = Date.now();
    void reload();
  };
  window.addEventListener("focus", onFocus);
  // An idle engine sends no ticks: refresh the dashboard numbers (disk space, totals) now and then anyway.
  const idle = setInterval(() => {
    if (document.visibilityState !== "visible" || Date.now() - lastTick < IDLE_REFRESH_MS) return;
    api.getMetrics().then(
      (m) => Date.now() - lastTick >= IDLE_REFRESH_MS && store.setMetrics(m),
      () => {},
    );
  }, IDLE_REFRESH_MS);
  return () => {
    unlisten();
    clearInterval(idle);
    document.removeEventListener("visibilitychange", onVisible);
    window.removeEventListener("focus", onFocus);
  };
}

/** Run an action; turn a failure into a toast instead of an unhandled rejection. */
export async function act<T>(fn: () => Promise<T>, okText?: string): Promise<T | undefined> {
  try {
    const r = await fn();
    if (okText) store.toast("info", okText);
    return r;
  } catch (e) {
    store.toast("error", normalizeError(e).message);
    return undefined;
  }
}

export interface DetailState {
  detail: DownloadDetail | null;
  /** This download's recent speed samples, newest last. */
  speeds: number[];
  error: string | null;
}

/**
 * Poll one download's detail (chunks, metadata) while its view is open. 4 Hz is plenty: the segment
 * monitor animates between samples, so the UI never needs to follow individual network writes.
 * Polling pauses while the window is hidden.
 */
export function useDetail(id: number, everyMs = 250): DetailState {
  const [state, setState] = useState<DetailState>({ detail: null, speeds: [], error: null });
  const speeds = useRef<number[]>([]);
  useEffect(() => {
    let alive = true;
    let busy = false;
    speeds.current = [];
    const poll = async () => {
      if (busy || document.visibilityState !== "visible") return;
      busy = true;
      try {
        const detail = await api.getDownloadDetail(id);
        if (!alive) return;
        speeds.current = [...speeds.current, detail.download.speedBps].slice(-90);
        setState({ detail, speeds: speeds.current, error: null });
      } catch (e) {
        if (alive) setState((s) => ({ ...s, error: normalizeError(e).message }));
      } finally {
        busy = false;
      }
    };
    void poll();
    const t = setInterval(poll, everyMs);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, [id, everyMs]);
  return state;
}
