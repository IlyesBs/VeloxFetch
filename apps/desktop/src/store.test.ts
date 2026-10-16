import { describe, expect, it } from "vitest";
import { createStore, initialState, reduce, SPEED_HISTORY_LEN } from "./store";
import type { Download, MediaJob, Metrics, PendingDownload, Tick } from "./types";

const dl = (id: number, over: Partial<Download> = {}): Download => ({
  id,
  url: `https://x.test/${id}`,
  filename: `f${id}.bin`,
  destination: "/tmp",
  status: "downloading",
  totalSize: 1000,
  downloaded: 0,
  speedBps: 0,
  etaSecs: null,
  connections: 0,
  priority: "normal",
  category: null,
  speedLimit: null,
  resumable: true,
  error: null,
  errorDetail: null,
  needs: null,
  errorCode: null,
  kind: "other",
  idle: false,
  queueId: 1,
  position: id * 1024,
  startAt: null,
  retrying: false,
  held: false,
  doctor: null,
  ...over,
});

const metrics = (speedBps: number): Metrics => ({
  speedBps,
  active: 1,
  queued: 0,
  paused: 0,
  failed: 0,
  completed: 0,
  totalDownloaded: 0,
  sessionDownloaded: 0,
  networkTotal: 0,
  networkToday: 0,
  networkMonth: 0,
  diskFree: null,
  diskTotal: null,
  folderBytes: null,
  globalLimit: null,
  netDownBps: null,
  netUpBps: null,
  diskReadBps: null,
  diskWriteBps: null,
  ioSample: 0,
});

const tick = (downloads: Tick["downloads"], speed = 0): { type: "tick"; tick: Tick } => ({
  type: "tick",
  tick: { metrics: metrics(speed), downloads },
});

describe("reduce", () => {
  it("adds, updates and removes downloads and keeps the groups right", () => {
    let s = initialState();
    s = reduce(s, { type: "added", download: dl(1, { status: "queued" }) });
    s = reduce(s, { type: "added", download: dl(2, { status: "downloading" }) });
    expect(s.ids).toEqual([1, 2]);
    expect(s.groups.queued).toEqual([1]);
    expect(s.groups.active).toEqual([2]);

    s = reduce(s, { type: "updated", download: dl(1, { status: "downloading" }) });
    expect(s.groups.active).toEqual([1, 2]);
    expect(s.groups.queued).toEqual([]);

    s = reduce(s, { type: "updated", download: dl(2, { status: "completed" }) });
    expect(s.groups.completed).toEqual([2]);

    s = reduce(s, { type: "removed", id: 1 });
    expect(s.ids).toEqual([2]);
    expect(s.byId[1]).toBeUndefined();
    expect(reduce(s, { type: "removed", id: 999 })).toBe(s);
  });

  it("does not rebuild groups on progress ticks (structural sharing)", () => {
    let s = reduce(initialState(), { type: "added", download: dl(1) });
    const groups = s.groups;
    s = reduce(s, tick([{ id: 1, downloaded: 100, totalSize: 1000, speedBps: 50, etaSecs: 18, connections: 4, retrying: false }], 50));
    expect(s.groups).toBe(groups);
    expect(s.byId[1]).toMatchObject({ downloaded: 100, speedBps: 50, etaSecs: 18, connections: 4, retrying: false });
    expect(s.metrics.speedBps).toBe(50);
  });

  it("keeps a download's object identity when nothing about it changed (lets React.memo skip it)", () => {
    let s = reduce(initialState(), { type: "added", download: dl(1) });
    s = reduce(s, { type: "added", download: dl(2) });
    const p = (id: number, downloaded: number) => ({ id, downloaded, totalSize: 1000, speedBps: 10, etaSecs: 5, connections: 2, retrying: false });
    s = reduce(s, tick([p(1, 100), p(2, 100)]));
    const [a, b] = [s.byId[1], s.byId[2]];
    s = reduce(s, tick([p(1, 200), p(2, 100)])); // only #1 moved
    expect(s.byId[1]).not.toBe(a);
    expect(s.byId[2]).toBe(b);
    const same = reduce(s, tick([p(1, 200), p(2, 100)]));
    expect(same.byId).toBe(s.byId); // nothing changed at all: same map
  });

  it("ignores ticks for unknown downloads and caps the speed history", () => {
    let s = reduce(initialState(), { type: "added", download: dl(1) });
    s = reduce(s, tick([{ id: 42, downloaded: 1, totalSize: 2, speedBps: 3, etaSecs: 4, connections: 5, retrying: false }]));
    expect(s.byId[42]).toBeUndefined();
    for (let i = 0; i < SPEED_HISTORY_LEN + 25; i++) s = reduce(s, tick([], i));
    expect(s.speedHistory).toHaveLength(SPEED_HISTORY_LEN);
    expect(s.speedHistory.at(-1)).toBe(SPEED_HISTORY_LEN + 24);
  });
});

describe("store", () => {
  it("coalesces any number of events per frame into a single notification", () => {
    const frames: Array<() => void> = [];
    const store = createStore((cb) => frames.push(cb));
    let notified = 0;
    store.subscribe(() => notified++);

    store.dispatch({ type: "added", download: dl(1) });
    for (let i = 1; i <= 200; i++) {
      store.dispatch(tick([{ id: 1, downloaded: i, totalSize: 1000, speedBps: i, etaSecs: null, connections: 1, retrying: false }], i));
    }
    expect(frames).toHaveLength(1); // one frame scheduled, not 201
    expect(notified).toBe(0); // nothing rendered until the frame
    frames.shift()!();
    expect(notified).toBe(1); // ... and then exactly one render
    expect(store.getState().byId[1]!.downloaded).toBe(200); // the last value wins

    store.dispatch(tick([], 5));
    expect(frames).toHaveLength(1); // scheduling works again for the next frame
  });

  it("load() replaces state and discards stale queued events", () => {
    const frames: Array<() => void> = [];
    const store = createStore((cb) => frames.push(cb));
    store.dispatch({ type: "added", download: dl(9) });
    store.load([dl(1), dl(2, { status: "paused" })], metrics(0));
    frames.shift()?.();
    const s = store.getState();
    expect(s.ready).toBe(true);
    expect(s.ids).toEqual([1, 2]);
    expect(s.byId[9]).toBeUndefined();
    expect(s.groups.paused).toEqual([2]);
  });

  it("keeps at most 4 toasts and dismisses by id", () => {
    const store = createStore(() => {});
    for (let i = 0; i < 7; i++) store.toast("error", `e${i}`);
    const t = store.getState().toasts;
    expect(t).toHaveLength(4);
    expect(t.at(-1)!.text).toBe("e6");
    store.dismissToast(t[0]!.id);
    expect(store.getState().toasts).toHaveLength(3);
  });

  it("keeps browser downloads that wait for the user, updates them in place and drops them when answered", () => {
    const p = (token: number, over: Partial<PendingDownload> = {}): PendingDownload => ({
      token, url: "https://x.test/a.iso", host: "x.test", filename: "a.iso", size: null, contentType: null,
      resumable: null, saveDir: "/dl", probing: true, incognito: false, createdAtMs: 0, media: null, analysis: null, ...over,
    });
    let s = initialState();
    s = reduce(s, { type: "pending", pending: p(1) });
    s = reduce(s, { type: "pending", pending: p(2) });
    s = reduce(s, { type: "pending", pending: p(1, { probing: false, size: 5 }) });
    expect(s.pending.map((x) => x.token)).toEqual([1, 2]);
    expect(s.pending[0]).toMatchObject({ size: 5, probing: false });
    s = reduce(s, { type: "pendingRemoved", token: 1 });
    expect(s.pending.map((x) => x.token)).toEqual([2]);
    expect(reduce(s, { type: "pendingRemoved", token: 99 })).toBe(s);
    // a resync keeps or replaces the waiting list, never loses it by accident
    const store = createStore(() => {});
    store.load([], metrics(0), null, [p(7)]);
    store.load([], metrics(0), null);
    expect(store.getState().pending.map((x) => x.token)).toEqual([7]);
  });

  it("keeps stream jobs in their own list, separate from downloads", () => {
    const job = (id: number, over: Partial<MediaJob> = {}): MediaJob => ({
      id,
      title: `Show ${id}`,
      host: "cdn.test",
      status: "downloading",
      bytes: 0,
      segmentsDone: 0,
      segmentsTotal: 10,
      site: false,
      percent: 0,
      folder: "/tmp",
      files: [],
      notes: [],
      error: null,
      errorCode: null,
      speedBps: 0,
      etaSecs: null,
      resumable: false,
      playable: false,
      ...over,
    });
    let s = initialState();
    s = reduce(s, { type: "media", job: job(1) });
    s = reduce(s, { type: "media", job: job(2) });
    expect(s.media.map((j) => j.id)).toEqual([1, 2]);
    expect(s.ids).toEqual([]);
    expect(s.groups.active).toEqual([]);
    s = reduce(s, { type: "media", job: job(1, { percent: 50, segmentsDone: 5 }) });
    expect(s.media).toHaveLength(2);
    expect(s.media[0]!.percent).toBe(50);
    expect(reduce(s, { type: "mediaRemoved", id: 1 }).media.map((j) => j.id)).toEqual([2]);
    expect(reduce(s, { type: "mediaRemoved", id: 99 })).toBe(s);
    // a resync replaces or keeps the list, never loses it by accident
    const store = createStore(() => {});
    store.load([], metrics(0), null, undefined, [job(7)]);
    store.load([], metrics(0), null);
    expect(store.getState().media.map((j) => j.id)).toEqual([7]);
  });
});
