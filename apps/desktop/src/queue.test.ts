import { describe, expect, it } from "vitest";
import { scheduleProblem } from "./components/ScheduleSettings";
import { formatWhen, secondsLeft } from "./components/when";
import { createStore, initialState, reduce } from "./store";
import type { QueueState, Schedule } from "./types";

const queue = (over: Partial<QueueState> = {}): QueueState => ({
  maxActive: 3,
  maxConnections: 16,
  paused: false,
  gateOpen: true,
  running: 0,
  waiting: 0,
  held: 0,
  schedule: { enabled: false, start: "22:00", stop: "07:00", days: Array(7).fill(true) },
  scheduleOpen: true,
  nextChange: null,
  power: { action: null, phase: "off", countdownEndsAtMs: null, graceSecs: 60, supported: ["shutdown", "sleep"], lastError: null },
  ...over,
});

describe("queue state in the store", () => {
  it("starts empty and is replaced by queue events without touching downloads", () => {
    const s0 = initialState();
    expect(s0.queue).toBeNull();
    const s1 = reduce(s0, { type: "queue", queue: queue({ paused: true }) });
    expect(s1.queue?.paused).toBe(true);
    expect(s1.byId).toBe(s0.byId);
    expect(s1.groups).toBe(s0.groups);
  });

  it("load keeps an earlier queue state when none is supplied, and takes a new one when given", () => {
    const store = createStore((cb) => cb());
    store.dispatch({ type: "queue", queue: queue({ maxActive: 5 }) });
    store.load([], reduce(initialState(), { type: "removed", id: 1 }).metrics);
    expect(store.getState().queue?.maxActive).toBe(5);
    store.load([], store.getState().metrics, queue({ maxActive: 2 }));
    expect(store.getState().queue?.maxActive).toBe(2);
  });
});

describe("schedule form validation", () => {
  const base: Schedule = { enabled: true, start: "22:00", stop: "07:00", days: Array(7).fill(true) };
  it("accepts an overnight window", () => expect(scheduleProblem(base)).toBeNull());
  it("rejects equal times, bad times and no days", () => {
    expect(scheduleProblem({ ...base, stop: "22:00" })).toMatch(/differ/);
    expect(scheduleProblem({ ...base, start: "" })).toMatch(/HH:MM/);
    expect(scheduleProblem({ ...base, start: "25:00" })).toMatch(/HH:MM/);
    expect(scheduleProblem({ ...base, days: Array(7).fill(false) })).toMatch(/day/);
  });
});

describe("time helpers", () => {
  it("formats the backend's local timestamp and passes through anything unexpected", () => {
    expect(formatWhen("2026-03-02T22:05")).toMatch(/22:05$/);
    expect(formatWhen("not a time")).toBe("not a time");
  });
  it("counts down in whole seconds and never goes negative", () => {
    expect(secondsLeft(10_000, 9_001)).toBe(1);
    expect(secondsLeft(10_000, 9_000)).toBe(1);
    expect(secondsLeft(10_000, 10_000)).toBe(0);
    expect(secondsLeft(10_000, 20_000)).toBe(0);
  });
});
