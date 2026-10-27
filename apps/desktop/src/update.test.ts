import { describe, expect, it, vi } from "vitest";
import type { UpdateStatus } from "./types";

// The panel's module imports the API (Tauri) and the store: plain stand-ins are enough for its text.
vi.mock("./api", () => ({ api: {}, normalizeError: (e: unknown) => ({ message: String(e) }) }));

const { updateLine } = await import("./components/UpdateSettings");
const { t } = await import("./i18n");
const { initialState, reduce } = await import("./store");

const base: UpdateStatus = {
  supported: true,
  autoCheck: true,
  phase: "idle",
  current: "1.0.0",
  version: null,
  size: null,
  received: 0,
  total: 0,
  checkedAt: null,
  error: null,
  install: "restart",
  releasePage: "https://github.com/IlyesBs/VeloxFetch/releases",
};

describe("updates", () => {
  it("says what the update state is, in words", () => {
    expect(updateLine(t, base)).toBe(t("upd.notChecked"));
    expect(updateLine(t, { ...base, phase: "upToDate" })).toBe(t("upd.upToDate"));
    expect(updateLine(t, { ...base, phase: "available", version: "1.2.0", size: 1048576 })).toContain("1.2.0");
    expect(updateLine(t, { ...base, phase: "downloading", received: 50, total: 200 })).toContain("25");
    expect(updateLine(t, { ...base, phase: "ready", version: "1.2.0" })).toBe(t("upd.ready", { v: "1.2.0" }));
    expect(updateLine(t, { ...base, phase: "elsewhere", version: "1.2.0" })).toBe(t("upd.elsewhere", { v: "1.2.0" }));
  });

  it("names every refusal without passing on anything from the network", () => {
    for (const error of ["network", "signature", "malformed", "rejected", "stale", "corrupt", "disk", "install"] as const) {
      const line = updateLine(t, { ...base, phase: "failed", error });
      expect(line.length).toBeGreaterThan(10);
      expect(line).not.toMatch(/upd\.err/);
    }
    expect(updateLine(t, { ...base, supported: false })).toBe(t("upd.unsupported"));
  });

  it("keeps the newest state from events", () => {
    const s = reduce(initialState(), { type: "update", update: { ...base, phase: "available", version: "2.0.0" } });
    expect(s.update?.version).toBe("2.0.0");
    expect(reduce(s, { type: "update", update: base }).update?.phase).toBe("idle");
  });
});
