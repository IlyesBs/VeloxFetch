// Frontend store. Designed for a 60 FPS UI:
//
//   Rust metrics aggregator --(one batched "tick", 5/s)--> events queue --(one flush per frame)--> React
//
//  * Events are queued and applied together once per animation frame, so however many arrive,
//    React renders at most once per frame.
//  * State is immutable with structural sharing: a download's object only changes when one of its
//    values changed, so `React.memo` cards skip re-rendering untouched rows.
//  * `groups` (which downloads are active / queued / ...) is rebuilt only when a download is added,
//    removed or changes status, never on progress ticks.

import type { Download, DownloadStatus, ManagerEvent, MediaJob, Metrics, PendingDownload, Profiles, Progress, Queues, QueueState } from "./types";

export type GroupName = "active" | "queued" | "paused" | "failed" | "completed" | "cancelled";
export type Groups = Record<GroupName, number[]>;

export interface Toast {
  id: number;
  kind: "error" | "info";
  text: string;
}

export interface State {
  ready: boolean;
  byId: Record<number, Download>;
  /** All ids, oldest first. */
  ids: number[];
  groups: Groups;
  metrics: Metrics;
  /** Total speed, newest last. Feeds the sparklines. */
  speedHistory: number[];
  /** Network and disk activity, newest last. */
  ioHistory: IoHistory;
  /** Queue / schedule / power state. Changes rarely and arrives as its own event. */
  queue: QueueState | null;
  /** The Smart Download Queues (their rules, open or closed, the network). Its own event too. */
  queues: Queues | null;
  /** Download Profiles (the active one, the list, what is in force). Its own event. */
  profiles: Profiles | null;
  /** Browser downloads waiting for the user (oldest first). */
  pending: PendingDownload[];
  /** Stream (HLS / DASH) downloads, oldest first. A separate list: they are not rows of the download list. */
  media: MediaJob[];
  toasts: Toast[];
}

export const SPEED_HISTORY_LEN = 90;

export const EMPTY_METRICS: Metrics = {
  speedBps: 0,
  active: 0,
  queued: 0,
  paused: 0,
  failed: 0,
  completed: 0,
  totalDownloaded: 0,
  sessionDownloaded: 0,
  diskFree: null,
  diskTotal: null,
  folderBytes: null,
  globalLimit: null,
  netDownBps: null,
  netUpBps: null,
  diskReadBps: null,
  diskWriteBps: null,
  ioSample: 0,
};

/** The side panels' charts: one point per network / disk reading (about one a second). */
export interface IoHistory {
  down: number[];
  up: number[];
  read: number[];
  write: number[];
}
const EMPTY_IO: IoHistory = { down: [], up: [], read: [], write: [] };

/** `io` with the reading in `m` added, if it is a new one. */
function withIo(io: IoHistory, prev: Metrics, m: Metrics): IoHistory {
  if (m.ioSample === prev.ioSample || m.ioSample === 0) return io;
  const add = (xs: number[], v: number | null) => [...xs, v ?? 0].slice(-SPEED_HISTORY_LEN);
  return { down: add(io.down, m.netDownBps), up: add(io.up, m.netUpBps), read: add(io.read, m.diskReadBps), write: add(io.write, m.diskWriteBps) };
}

const GROUP_OF: Record<DownloadStatus, GroupName> = {
  downloading: "active",
  verifying: "active",
  queued: "queued",
  paused: "paused",
  failed: "failed",
  completed: "completed",
  cancelled: "cancelled",
};

export function groupOf(status: DownloadStatus): GroupName {
  return GROUP_OF[status];
}

function emptyGroups(): Groups {
  return { active: [], queued: [], paused: [], failed: [], completed: [], cancelled: [] };
}

export function buildGroups(ids: number[], byId: Record<number, Download>): Groups {
  const g = emptyGroups();
  for (const id of ids) {
    const d = byId[id];
    if (d) g[groupOf(d.status)].push(id);
  }
  return g;
}

export function initialState(): State {
  return {
    ready: false,
    byId: {},
    ids: [],
    groups: emptyGroups(),
    metrics: EMPTY_METRICS,
    speedHistory: [],
    ioHistory: EMPTY_IO,
    queue: null,
    queues: null,
    profiles: null,
    pending: [],
    media: [],
    toasts: [],
  };
}

/** Same object back if nothing changed, so memoised rows keep their identity. */
function mergeProgress(d: Download, p: Progress): Download {
  const total = p.totalSize ?? d.totalSize;
  if (
    d.downloaded === p.downloaded &&
    d.totalSize === total &&
    d.speedBps === p.speedBps &&
    d.etaSecs === p.etaSecs &&
    d.connections === p.connections &&
    d.retrying === (p.retrying ?? false)
  ) {
    return d;
  }
  return {
    ...d,
    downloaded: p.downloaded,
    totalSize: total,
    speedBps: p.speedBps,
    etaSecs: p.etaSecs,
    connections: p.connections,
    retrying: p.retrying ?? false,
  };
}

export function reduce(state: State, ev: ManagerEvent): State {
  switch (ev.type) {
    case "added":
    case "updated": {
      const d = ev.download;
      const prev = state.byId[d.id];
      const byId = { ...state.byId, [d.id]: d };
      const isNew = prev === undefined;
      const ids = isNew ? [...state.ids, d.id] : state.ids;
      const groups = isNew || prev.status !== d.status ? buildGroups(ids, byId) : state.groups;
      return { ...state, byId, ids, groups };
    }
    case "removed": {
      if (!(ev.id in state.byId)) return state;
      const byId = { ...state.byId };
      delete byId[ev.id];
      const ids = state.ids.filter((i) => i !== ev.id);
      return { ...state, byId, ids, groups: buildGroups(ids, byId) };
    }
    case "tick": {
      let byId = state.byId;
      for (const p of ev.tick.downloads) {
        const d = byId[p.id];
        if (!d) continue;
        const next = mergeProgress(d, p);
        if (next !== d) {
          if (byId === state.byId) byId = { ...state.byId };
          byId[p.id] = next;
        }
      }
      const speedHistory = [...state.speedHistory, ev.tick.metrics.speedBps].slice(-SPEED_HISTORY_LEN);
      return { ...state, byId, metrics: ev.tick.metrics, speedHistory, ioHistory: withIo(state.ioHistory, state.metrics, ev.tick.metrics) };
    }
    case "queue":
      return { ...state, queue: ev.queue };
    case "queues":
      return { ...state, queues: ev.queues };
    case "profiles":
      return { ...state, profiles: ev.profiles };
    case "pending": {
      const i = state.pending.findIndex((p) => p.token === ev.pending.token);
      const pending = i < 0 ? [...state.pending, ev.pending] : state.pending.map((p, j) => (j === i ? ev.pending : p));
      return { ...state, pending };
    }
    case "media": {
      const i = state.media.findIndex((j) => j.id === ev.job.id);
      const media = i < 0 ? [...state.media, ev.job] : state.media.map((j, k) => (k === i ? ev.job : j));
      return { ...state, media };
    }
    case "mediaRemoved":
      return state.media.some((j) => j.id === ev.id) ? { ...state, media: state.media.filter((j) => j.id !== ev.id) } : state;
    case "pendingRemoved":
      return state.pending.some((p) => p.token === ev.token) ? { ...state, pending: state.pending.filter((p) => p.token !== ev.token) } : state;
  }
}

type Listener = () => void;
type Scheduler = (cb: () => void) => void;

const rafOrTimeout: Scheduler = (cb) =>
  typeof requestAnimationFrame === "function" ? void requestAnimationFrame(() => cb()) : void setTimeout(cb, 16);

export interface Store {
  getState(): State;
  subscribe(l: Listener): () => void;
  /** Queue an engine event; applied (with all others) at the next frame. */
  dispatch(ev: ManagerEvent): void;
  /** Apply queued events now. */
  flush(): void;
  /** Replace everything with a fresh snapshot (initial load / resync). */
  load(
    downloads: Download[],
    metrics: Metrics,
    queue?: QueueState | null,
    waiting?: PendingDownload[],
    media?: MediaJob[],
    queues?: Queues | null,
  ): void;
  setMetrics(m: Metrics): void;
  toast(kind: Toast["kind"], text: string): void;
  dismissToast(id: number): void;
}

export function createStore(schedule: Scheduler = rafOrTimeout): Store {
  let state = initialState();
  let pending: ManagerEvent[] = [];
  let scheduled = false;
  let toastSeq = 0;
  const listeners = new Set<Listener>();
  const notify = () => listeners.forEach((l) => l());

  const set = (s: State) => {
    if (s !== state) {
      state = s;
      notify();
    }
  };

  const flush = () => {
    scheduled = false;
    if (pending.length === 0) return;
    let s = state;
    for (const ev of pending) s = reduce(s, ev);
    pending = [];
    set(s);
  };

  return {
    getState: () => state,
    subscribe(l) {
      listeners.add(l);
      return () => void listeners.delete(l);
    },
    dispatch(ev) {
      pending.push(ev);
      if (!scheduled) {
        scheduled = true;
        schedule(flush);
      }
    },
    flush,
    load(downloads, metrics, queue, waiting, media, queues) {
      const byId: Record<number, Download> = {};
      for (const d of downloads) byId[d.id] = d;
      const ids = downloads.map((d) => d.id);
      pending = [];
      set({ ...state, ready: true, byId, ids, groups: buildGroups(ids, byId), metrics, queue: queue ?? state.queue, queues: queues ?? state.queues, pending: waiting ?? state.pending, media: media ?? state.media });
    },
    setMetrics: (metrics) => set({ ...state, metrics, ioHistory: withIo(state.ioHistory, state.metrics, metrics) }),
    toast(kind, text) {
      const id = ++toastSeq;
      set({ ...state, toasts: [...state.toasts.slice(-3), { id, kind, text }] });
    },
    dismissToast: (id) => set({ ...state, toasts: state.toasts.filter((t) => t.id !== id) }),
  };
}
