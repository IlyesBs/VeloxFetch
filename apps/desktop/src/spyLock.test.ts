import { afterEach, describe, expect, it, vi } from "vitest";
import { createSpyLock } from "./spyLock";

afterEach(() => vi.useRealTimers());

describe("the Settings side menu after a click", () => {
  it("keeps the clicked section while the page scrolls there, then follows the page again", () => {
    vi.useFakeTimers();
    const lock = createSpyLock(200);
    expect(lock.scrolled()).toBe(false);
    lock.pin("network");
    for (let i = 0; i < 10; i++) {
      vi.advanceTimersByTime(16); // a smooth scroll: an event every frame
      expect(lock.scrolled()).toBe(true);
    }
    vi.advanceTimersByTime(250); // the scrolling stopped
    expect(lock.scrolled()).toBe(false);
  });

  it("follows the page at once when the user scrolls by hand", () => {
    vi.useFakeTimers();
    const lock = createSpyLock(200);
    lock.pin("network");
    lock.release();
    expect(lock.scrolled()).toBe(false);
  });

  it("is used by the Settings page", async () => {
    const fs = await import("node:fs");
    const src = fs.readFileSync(new URL("./views/SettingsView.tsx", import.meta.url), "utf8");
    expect(src).toMatch(/if \(lock\.current\.scrolled\(\)\) return;/);
    expect(src).toMatch(/lock\.current\.pin\(id\);/);
    expect(src).toMatch(/"wheel", "touchstart", "keydown"/);
  });
});
