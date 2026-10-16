import { beforeEach, describe, expect, it, vi } from "vitest";
import type { SetGameMode, SetSmartThrottle, SmartThrottle } from "./types";

// throttle.ts marks <html> while it is on: a bare stand-in is enough here (no browser in these tests).
(globalThis as unknown as { document: unknown }).document = { documentElement: { dataset: {} }, visibilityState: "visible" };

/** The app's side: answers set/get by hand, in whatever order a test wants. */
const server = vi.hoisted(() => ({
  state: null as unknown as SmartThrottle,
  sets: [] as { patch: SetSmartThrottle; resolve: () => void; reject: (e: unknown) => void }[],
  gets: [] as { resolve: () => void }[],
  games: [] as { patch: SetGameMode; resolve: () => void; reject: (e: unknown) => void }[],
  toasts: [] as string[],
}));

vi.mock("./api", () => ({
  api: {
    setSmartThrottle: (patch: SetSmartThrottle) =>
      new Promise((resolve, reject) => {
        server.sets.push({
          patch,
          resolve: () => {
            server.state = { ...server.state, ...patch };
            resolve(server.state);
          },
          reject,
        });
      }),
    setGameMode: (patch: SetGameMode) =>
      new Promise((resolve, reject) => {
        server.games.push({
          patch,
          resolve: () => {
            const on = patch.on ?? server.state.gameMode.on;
            server.state = {
              ...server.state,
              reason: on ? "game" : "off",
              gameMode: { ...server.state.gameMode, on, untilMs: on ? 1_000_000 : null },
            };
            resolve(server.state);
          },
          reject,
        });
      }),
    getSmartThrottle: () => new Promise((resolve) => server.gets.push({ resolve: () => resolve({ ...server.state }) })),
  },
  normalizeError: (e: unknown) => ({ message: String(e) }),
}));
vi.mock("./hooks", () => ({
  store: { toast: (_k: string, m: string) => server.toasts.push(m), getState: () => ({ metrics: { speedBps: 0 } }) },
}));

vi.mock("./chrome", () => ({ syncFrameTheme: () => {} }));

const { throttleStore, gaugeShare, throttleStatus, throttleOn, throttleLevel, LEVEL_REQUEST, gameOn, gameSecondsLeft, formatLeft, gameLine } =
  await import("./throttle");
const { paletteKey } = await import("./theme");

const base: SmartThrottle = {
  enabled: false,
  apps: [],
  sharePercent: 25,
  activeApp: null,
  limitBps: null,
  scanOk: true,
  auto: true,
  minBps: 262144,
  reason: "off",
  otherBps: null,
  userIdle: null,
  peakBps: null,
  referenceBps: null,
  gamePolicy: "adaptive",
  gameLimitBps: 524288,
  gameHold: false,
  profileAdaptive: null,
  heldByNetworks: false,
  gameMode: { on: false, untilMs: null, minutes: 120 },
};
const tick = () => new Promise((r) => setTimeout(r, 0));

beforeEach(async () => {
  server.state = { ...base };
  server.sets = [];
  server.gets = [];
  server.toasts = [];
  server.games = [];
  throttleStore.set({ ...base });
});

describe("the window does not flash while Smart throttle is on", () => {
  it("polls that bring the same state never touch <html> again", () => {
    // Each write to the attribute told the theme watchers to repaint the whole window background (every 3 s).
    let writes = 0;
    const marks: Record<string, string> = {};
    const root = (globalThis as unknown as { document: { documentElement: { dataset: unknown } } }).document.documentElement;
    const before = root.dataset;
    root.dataset = new Proxy(marks, {
      set: (t, k, v) => {
        writes++;
        t[k as string] = v;
        return true;
      },
      deleteProperty: (t, k) => {
        writes++;
        delete t[k as string];
        return true;
      },
    });
    try {
      const on = { ...base, enabled: true, reason: "fullFree" as const };
      for (let i = 0; i < 20; i++) throttleStore.set(on);
      expect(marks.throttle).toBe("on");
      expect(writes).toBe(1);
      const limiting = { ...on, limitBps: 800_000, reason: "app" as const };
      for (let i = 0; i < 20; i++) throttleStore.set(limiting);
      expect(marks.throttle).toBe("limiting");
      expect(writes).toBe(2);
      throttleStore.set({ ...base });
      expect(marks.throttle).toBeUndefined();
      expect(writes).toBe(3);
    } finally {
      root.dataset = before;
    }
  });

  it("slowing downloads or not keeps the same colours (no repaint)", () => {
    expect(paletteKey(null, "on", false)).toBe(paletteKey(null, "limiting", false));
    expect(paletteKey("dark", "on", true)).toBe(paletteKey("dark", "limiting", false));
    expect(paletteKey(null, "on", false)).not.toBe(paletteKey(null, null, false));
    expect(paletteKey(null, null, false)).not.toBe(paletteKey(null, null, true));
    expect(paletteKey("light", null, false)).not.toBe(paletteKey("dark", null, false));
  });
});

describe("Smart throttle state in the window", () => {
  it("a change shows at once, before the app answers", async () => {
    void throttleStore.save({ enabled: true });
    expect(throttleStore.get()?.enabled).toBe(true);
    await tick();
    server.sets[0]!.resolve();
    await tick();
    expect(throttleStore.get()?.enabled).toBe(true);
  });

  it("a poll answered after a change never brings the old state back (no flicker)", async () => {
    const poll = throttleStore.refresh(); // asked before the click
    await tick();
    const saved = throttleStore.save({ enabled: true });
    server.gets[0]!.resolve(); // the old answer arrives: enabled false
    await poll;
    expect(throttleStore.get()?.enabled).toBe(true);
    await tick();
    server.sets[0]!.resolve();
    await saved;
    expect(throttleStore.get()?.enabled).toBe(true);
    // While a change is on its way, no poll is sent at all.
    void throttleStore.save({ sharePercent: 10 });
    await throttleStore.refresh();
    expect(server.gets.length).toBe(1);
    await tick();
    server.sets[1]!.resolve();
  });

  it("quick clicks are sent in order and the last one wins", async () => {
    const a = throttleStore.save({ enabled: true });
    const b = throttleStore.save({ enabled: false });
    const c = throttleStore.save({ enabled: true, sharePercent: 50 });
    expect(throttleStore.get()).toMatchObject({ enabled: true, sharePercent: 50 });
    for (let i = 0; i < 3; i++) {
      await tick();
      expect(server.sets.length).toBe(i + 1); // one at a time
      server.sets[i]!.resolve();
    }
    await Promise.all([a, b, c]);
    expect(server.sets.map((s) => s.patch.enabled)).toEqual([true, false, true]);
    expect(throttleStore.get()).toMatchObject({ enabled: true, sharePercent: 50 });
    expect(server.state).toMatchObject({ enabled: true, sharePercent: 50 });
  });

  it("turning it off shows full speed at once; a refused change brings the real state back", async () => {
    throttleStore.set({ ...base, enabled: true, limitBps: 1_000_000, reason: "browsing" });
    void throttleStore.save({ enabled: false });
    expect(throttleStore.get()).toMatchObject({ enabled: false, limitBps: null, reason: "off" });
    await tick();
    server.sets[0]!.resolve();
    await tick();

    const bad = throttleStore.save({ minBps: 1 });
    expect(throttleStore.get()?.minBps).toBe(1);
    await tick();
    server.sets[1]!.reject("the reserved minimum must be between 32 KB/s and 1 GB/s");
    await bad;
    await tick();
    server.gets[0]!.resolve();
    await tick();
    expect(throttleStore.get()?.minBps).toBe(base.minBps);
    expect(server.toasts.at(-1)).toMatch(/reserved minimum/);
  });

  it("the gauge shows the cap against the speed downloads usually get", () => {
    expect(gaugeShare(null)).toBe(1);
    expect(gaugeShare({ ...base, enabled: true })).toBe(1);
    expect(gaugeShare({ ...base, enabled: true, limitBps: 500, referenceBps: 1000, peakBps: 100_000 })).toBe(0.5);
    // No speed seen yet: never "100 % while limiting"; the share the mode keeps.
    expect(gaugeShare({ ...base, enabled: true, limitBps: 500, reason: "app" })).toBe(0.25);
    expect(gaugeShare({ ...base, enabled: true, limitBps: 500, reason: "browsing" })).toBe(0.5);
    expect(gaugeShare({ ...base, enabled: true, limitBps: 10, referenceBps: 1_000_000 })).toBe(0.04);
  });

  it("a game's policy is shown as it is: limited to a speed, or downloads paused until it closes", () => {
    const t = (k: string, v?: Record<string, string | number>) => `${k} ${JSON.stringify(v ?? {})}`;
    const playing: SmartThrottle = { ...base, enabled: true, reason: "app", activeApp: "cs2", limitBps: 262144, gamePolicy: "low" };
    expect(throttleStatus(t, playing)).toMatch(/^thr\.app .*"speed":"256/);
    expect(throttleStatus(t, { ...playing, gamePolicy: "pause", gameHold: true })).toBe('thr.appPaused {"app":"cs2"}');
    // Never "a game" without a detected program.
    expect(throttleStatus(t, { ...base, enabled: true, reason: "fullFree" })).toMatch(/^thr\.fullFree/);
  });

  it("the policy is changed like any other setting: at once, and sent to the app", async () => {
    void throttleStore.save({ gamePolicy: "pause" });
    expect(throttleStore.get()?.gamePolicy).toBe("pause");
    await tick();
    expect(server.sets.map((x) => x.patch)).toEqual([{ gamePolicy: "pause" }]);
    server.sets[0]?.resolve();
    await tick();
    expect(server.state.gamePolicy).toBe("pause");
  });

  it("three levels: Fast (off, or the Fast profile), Smart (automatic) and Manual (the chosen level, always)", () => {
    const t = (k: string, v?: Record<string, string | number>) => `${k}${v ? JSON.stringify(v) : ""}`;
    const s = { ...base, enabled: true, auto: true } as SmartThrottle;
    expect(throttleLevel({ ...s, enabled: false })).toBe("fast");
    expect(throttleLevel(s)).toBe("smart");
    expect(throttleLevel({ ...s, auto: false })).toBe("manual");
    // The Fast profile holds everything off, whatever the user chose; Balanced's profile means Smart.
    expect(throttleLevel({ ...s, auto: false, profileAdaptive: false })).toBe("fast");
    expect(throttleStatus(t, { ...s, profileAdaptive: false })).toMatch(/^thr\.fastProfile/);
    expect(throttleLevel({ ...s, enabled: false, auto: false, profileAdaptive: true })).toBe("smart");
    // Manual: its level by name, and full speed until the connection's speed is known.
    const manual = { ...s, auto: false, sharePercent: 50, reason: "manual" as const };
    expect(throttleStatus(t, { ...manual, limitBps: 2_000_000 })).toMatch(/^thr\.manual\{"name":"hud\.performance"/);
    expect(throttleStatus(t, { ...manual, limitBps: null })).toMatch(/^thr\.manualMeasuring/);
    expect(LEVEL_REQUEST.fast).toEqual({ enabled: false });
    expect(LEVEL_REQUEST.manual).toEqual({ enabled: true, auto: false });
  });
});

describe("several networks at once", () => {
  it("holds Smart throttle off, whatever the user's switch or the profile say", () => {
    expect(throttleOn({ ...base, enabled: true })).toBe(true);
    expect(throttleOn({ ...base, enabled: true, heldByNetworks: true })).toBe(false);
    expect(throttleOn({ ...base, profileAdaptive: true, heldByNetworks: true })).toBe(false);
    // The line under the gauge and in Settings says why it is off (not just "off").
    const t = (k: string) => k;
    expect(throttleStatus(t, { ...base, enabled: true, heldByNetworks: true })).toBe("thr.heldByNetworks");
    expect(throttleStatus(t, { ...base, enabled: true, profileAdaptive: false, heldByNetworks: true })).toBe("thr.heldByNetworks");
    expect(throttleStatus(t, { ...base, enabled: false })).toBe("thr.off");
  });
});

describe("Game mode", () => {
  const t = (k: string, v?: Record<string, string | number>) => `${k}${v ? JSON.stringify(v) : ""}`;
  const playing: SmartThrottle = { ...base, reason: "game", limitBps: 262144, gamePolicy: "low", gameLimitBps: 262144, gameMode: { on: true, untilMs: 10_000, minutes: 120 } };

  it("wins over every level and over several networks at once, and says what downloads do", () => {
    expect(gameOn(playing)).toBe(true);
    expect(gameOn(base)).toBe(false);
    expect(throttleStatus(t, playing)).toMatch(/^gm\.name · gm\.lineLow\{"speed":"256/);
    expect(throttleStatus(t, { ...playing, heldByNetworks: true })).toMatch(/^gm\.name/);
    expect(throttleStatus(t, { ...playing, profileAdaptive: false })).toMatch(/^gm\.name/);
    expect(gameLine(t, { ...playing, gamePolicy: "pause" })).toBe("gm.linePause");
    expect(gameLine(t, { ...playing, gamePolicy: "adaptive" })).toMatch(/^gm\.lineAdaptive\{/);
    expect(gameLine(t, { ...playing, gamePolicy: "adaptive", limitBps: null })).toBe("gm.lineAdaptiveNoSpeed");
    // The gauge shows Game mode's cap even with Smart throttle off.
    expect(gaugeShare({ ...playing, referenceBps: 1_048_576 })).toBe(0.25);
  });

  it("time left: counts down, never below zero, and none when it lasts until turned off", () => {
    expect(gameSecondsLeft(playing.gameMode, 4_000)).toBe(6);
    expect(gameSecondsLeft(playing.gameMode, 20_000)).toBe(0);
    expect(gameSecondsLeft({ ...playing.gameMode, untilMs: null }, 0)).toBeNull();
    expect(formatLeft(t, 30)).toBe('dur.s{"s":30}');
    expect(formatLeft(t, 61)).toBe('dur.m{"m":2}');
    expect(formatLeft(t, 2 * 3600)).toBe('dur.hm{"h":2,"m":0}');
    expect(formatLeft(t, 6120)).toBe('dur.hm{"h":1,"m":42}');
  });

  it("one click shows it at once, then the app's answer; the whole window turns amber and says so", async () => {
    void throttleStore.game({ on: true }, "on!");
    const shown = throttleStore.get();
    expect(shown?.gameMode.on).toBe(true);
    expect(shown?.reason).toBe("game");
    expect(Math.abs((shown?.gameMode.untilMs ?? 0) - Date.now() - 120 * 60_000)).toBeLessThan(5_000);
    const root = (globalThis as unknown as { document: { documentElement: { dataset: Record<string, string> } } }).document.documentElement;
    expect(root.dataset.game).toBe("on");
    expect(root.dataset.throttle).toBeDefined();
    await tick();
    expect(server.games.map((x) => x.patch)).toEqual([{ on: true }]);
    server.games[0]?.resolve();
    await tick();
    expect(throttleStore.get()?.gameMode.untilMs).toBe(1_000_000);
    expect(server.toasts).toEqual(["on!"]);
    // Off: gone at once, colours back.
    const off = throttleStore.game({ on: false });
    expect(throttleStore.get()?.gameMode.on).toBe(false);
    expect(root.dataset.game).toBeUndefined();
    expect(root.dataset.throttle).toBeUndefined();
    await tick();
    server.games[1]?.resolve();
    await off;
  });

  it("until turned off shows no end; a refused change brings the real state back", async () => {
    const bad = throttleStore.game({ on: true, minutes: 0 });
    expect(throttleStore.get()?.gameMode.untilMs).toBeNull();
    await tick();
    server.games[0]?.reject("Game mode lasts at most 24 hours");
    await bad;
    await tick();
    expect(server.toasts).toEqual(["Game mode lasts at most 24 hours"]);
    expect(server.gets.length).toBe(1);
    server.gets[0]?.resolve();
    await tick();
    expect(throttleStore.get()?.gameMode.on).toBe(false);
  });
});
