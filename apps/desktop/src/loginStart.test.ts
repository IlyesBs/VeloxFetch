import { describe, expect, it } from "vitest";

// The component module reads <html> through i18n when it loads: a bare stand-in is enough here.
(globalThis as unknown as { document: unknown }).document = { documentElement: {} };
const { loginStartChoices } = await import("./components/DesktopSettings");

describe("how VeloxFetch starts at login", () => {
  it("offers the tray, minimized and the open window, the tray chosen by default", () => {
    expect(loginStartChoices(undefined, false)).toEqual({ current: "tray", offered: ["tray", "minimized", "window"] });
    expect(loginStartChoices("minimized", false).current).toBe("minimized");
    expect(loginStartChoices("window", false).current).toBe("window");
  });

  it("without a tray, never offers it and shows what really happens: the window opens", () => {
    expect(loginStartChoices("tray", true)).toEqual({ current: "window", offered: ["minimized", "window"] });
    expect(loginStartChoices(undefined, true).current).toBe("window");
    expect(loginStartChoices("minimized", true).current).toBe("minimized");
  });
});
