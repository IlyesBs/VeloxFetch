import { describe, expect, it } from "vitest";
import { queueProblem } from "./components/QueueEditor";
import {
  canMove,
  isPlain,
  minutesOf,
  nextDownload,
  NO_RULES,
  queueName,
  queueOrder,
  quickTimes,
  ruleChips,
  timelineBars,
  timelinePoint,
  visualState,
  waitReason,
} from "./smartQueue";
import { reduce, initialState } from "./store";
import type { Download, QueueInfo, Queues } from "./types";

const dl = (id: number, over: Partial<Download> = {}): Download => ({
  id,
  url: `https://x.test/${id}`,
  filename: `f${id}.bin`,
  destination: "/tmp",
  status: "queued",
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
  ...over,
});

const queue = (over: Partial<QueueInfo> = {}): QueueInfo => ({
  id: 1,
  name: "",
  rules: NO_RULES,
  paused: false,
  gate: "open",
  opensAt: null,
  forced: false,
  spans: [],
  ...over,
});

const queues = (list: QueueInfo[], over: Partial<Queues> = {}): Queues => ({
  queues: list,
  givingWay: false,
  idleKnown: true,
  online: true,
  connection: "wired",
  now: "2026-03-02T12:00",
  ...over,
});

const byId = (list: Download[]) => Object.fromEntries(list.map((d) => [d.id, d]));

describe("the state shown for each download", () => {
  it("names the seven states", () => {
    const q = queue();
    expect(visualState(dl(1, { status: "downloading" }), q)).toBe("downloading");
    expect(visualState(dl(1, { status: "downloading", retrying: true }), q)).toBe("retrying");
    expect(visualState(dl(1), q)).toBe("waiting");
    expect(visualState(dl(1, { startAt: "2026-03-02T23:00" }), q)).toBe("scheduled");
    expect(visualState(dl(1, { status: "paused" }), q)).toBe("paused");
    expect(visualState(dl(1, { status: "completed" }), q)).toBe("completed");
    expect(visualState(dl(1, { status: "failed" }), q)).toBe("failed");
  });

  it("a download held back by its queue's hours is scheduled, one paused by its queue's rules carries on by itself", () => {
    const night = queue({ gate: "time", opensAt: "2026-03-02T23:00" });
    expect(visualState(dl(1), night)).toBe("scheduled");
    expect(visualState(dl(1, { status: "paused", held: true }), night)).toBe("scheduled");
    expect(visualState(dl(1, { status: "paused", held: true }), queue({ gate: "notIdle" }))).toBe("waiting");
    // Paused by the user: stays paused.
    expect(visualState(dl(1, { status: "paused" }), night)).toBe("paused");
  });

  it("says why a download waits", () => {
    const all = queues([queue()]);
    expect(waitReason(dl(1), queue(), all)).toBe("slot");
    expect(waitReason(dl(1, { idle: true }), queue(), all)).toBe("later");
    expect(waitReason(dl(1, { idle: true }), queue({ rules: { ...NO_RULES, onlyWhenIdle: true } }), all)).toBe("slot");
    expect(waitReason(dl(1, { startAt: "2026-03-02T23:00" }), queue(), all)).toBe("at");
    expect(waitReason(dl(1), queue({ gate: "offline" }), all)).toBe("offline");
    expect(waitReason(dl(1, { priority: "low" }), queue(), queues([queue()], { givingWay: true }))).toBe("givingWay");
    expect(waitReason(dl(1, { priority: "high" }), queue(), queues([queue()], { givingWay: true }))).toBe("slot");
    expect(waitReason(dl(1, { status: "downloading" }), queue(), all)).toBeNull();
  });
});

describe("the queue order", () => {
  it("running first, then waiting by priority and place, then scheduled, paused, failed, finished", () => {
    const list = [
      dl(1, { status: "completed" }),
      dl(2, { status: "paused" }),
      dl(3, { priority: "low" }),
      dl(4, { status: "downloading" }),
      dl(5, { position: 1 }),
      dl(6, { priority: "high" }),
      dl(7, { startAt: "2026-03-03T02:00" }),
      dl(8, { startAt: "2026-03-02T23:00" }),
      dl(9, { status: "failed" }),
      dl(10, { status: "cancelled" }),
    ];
    expect(queueOrder(list.map((d) => d.id), byId(list), queue())).toEqual([4, 6, 5, 3, 8, 7, 2, 9, 1]);
  });

  it("only neighbours of the same priority and state trade places", () => {
    const list = [dl(1), dl(2), dl(3, { priority: "high" }), dl(4, { status: "downloading" })];
    const order = queueOrder(list.map((d) => d.id), byId(list), queue());
    expect(order).toEqual([4, 3, 1, 2]);
    expect(canMove(order, byId(list), 2, -1)).toBe(true);
    expect(canMove(order, byId(list), 1, -1)).toBe(false);
    expect(canMove(order, byId(list), 2, 1)).toBe(false);
    expect(canMove(order, byId(list), 4, 1)).toBe(false);
  });
});

describe("next download", () => {
  const q = queue();
  it("starts now when nothing runs, else when the first running one should finish", () => {
    const idle = [dl(1)];
    expect(nextDownload([1], byId(idle), q, queues([q]), idle)).toEqual({ id: 1, when: { kind: "now" } });
    const busy = [dl(1), dl(2, { status: "downloading", etaSecs: 90 }), dl(3, { status: "downloading", etaSecs: 30 })];
    expect(nextDownload([1], byId(busy), q, queues([q]), busy)).toEqual({ id: 1, when: { kind: "slot", etaSecs: 30 } });
  });
  it("uses the start time of a scheduled queue or download, and ignores downloads left for later", () => {
    const night = queue({ gate: "time", opensAt: "2026-03-02T23:00" });
    const list = [dl(1)];
    expect(nextDownload([1], byId(list), night, queues([night]), list)).toEqual({ id: 1, when: { kind: "at", at: "2026-03-02T23:00" } });
    const at = [dl(1, { startAt: "2026-03-03T02:00" })];
    expect(nextDownload([1], byId(at), q, queues([q]), at)).toEqual({ id: 1, when: { kind: "at", at: "2026-03-03T02:00" } });
    const later = [dl(1, { idle: true })];
    expect(nextDownload([1], byId(later), q, queues([q]), later)).toBeNull();
    const idleQ = queue({ gate: "notIdle" });
    expect(nextDownload([1], byId(list), idleQ, queues([idleQ]), list)?.when).toEqual({ kind: "reason", reason: "notIdle" });
  });
});

describe("timeline", () => {
  it("places windows and start times on the next 24 hours", () => {
    expect(minutesOf("2026-03-02T12:00")! - minutesOf("2026-03-02T11:00")!).toBe(60);
    expect(minutesOf("bad")).toBeNull();
    const bars = timelineBars([{ from: "2026-03-02T23:00", to: "2026-03-03T07:00" }], "2026-03-02T12:00");
    expect(bars).toHaveLength(1);
    expect(bars[0]!.left).toBeCloseTo((11 / 24) * 100);
    expect(bars[0]!.width).toBeCloseTo((8 / 24) * 100);
    expect(timelinePoint("2026-03-03T00:00", "2026-03-02T12:00")).toBeCloseTo(50);
    expect(timelinePoint("2026-03-04T00:00", "2026-03-02T12:00")).toBeNull();
  });
  it("offers quick start times that are never in the past", () => {
    const now = new Date(2026, 2, 2, 23, 30);
    const times = quickTimes(now).map((x) => x.at);
    expect(times.every((t) => t > "2026-03-02T23:30")).toBe(true);
    expect(times).toContain("2026-03-03T23:00");
  });
});

describe("rules", () => {
  it("knows a queue without rules and describes the rules in a few words", () => {
    expect(isPlain(NO_RULES)).toBe(true);
    expect(ruleChips(NO_RULES)).toEqual([]);
    const chips = ruleChips({ when: { kind: "between", start: "23:00", stop: "07:00", days: Array(7).fill(true) }, onlyWhenIdle: true, connection: "wifi", pauseAboveBps: 1e6, largeFilesOver: 1e9 });
    expect(chips).toHaveLength(5);
    expect(chips[0]).toContain("23:00");
    expect(queueName({ id: 1, name: "" })).toBe("Main queue");
    expect(queueName({ id: 2, name: "Night" })).toBe("Night");
  });
  it("checks the editor form before it is sent", () => {
    const days = Array(7).fill(true);
    expect(queueProblem("Night", NO_RULES, false)).toBeNull();
    expect(queueProblem("", NO_RULES, true)).toBeNull();
    expect(queueProblem(" ", NO_RULES, false)).toMatch(/name/);
    expect(queueProblem("x".repeat(41), NO_RULES, false)).toMatch(/40/);
    expect(queueProblem("N", { ...NO_RULES, when: { kind: "between", start: "23:00", stop: "23:00", days } }, false)).toMatch(/differ/);
    expect(queueProblem("N", { ...NO_RULES, when: { kind: "startAt", time: "2:00", days } }, false)).toMatch(/HH:MM/);
    expect(queueProblem("N", { ...NO_RULES, when: { kind: "startAt", time: "02:00", days: Array(7).fill(false) } }, false)).toMatch(/day/);
  });
});

describe("store", () => {
  it("keeps the queues from their own event, and the retrying flag from ticks", () => {
    const s1 = reduce(initialState(), { type: "queues", queues: queues([queue()]) });
    expect(s1.queues?.queues).toHaveLength(1);
    const s2 = reduce(s1, { type: "added", download: dl(1, { status: "downloading" }) });
    const metrics = s2.metrics;
    const s3 = reduce(s2, { type: "tick", tick: { metrics, downloads: [{ id: 1, downloaded: 5, totalSize: 1000, speedBps: 0, etaSecs: null, connections: 0, retrying: true }] } });
    expect(s3.byId[1]!.retrying).toBe(true);
  });
});
