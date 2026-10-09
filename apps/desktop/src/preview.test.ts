import { beforeEach, describe, expect, it, vi } from "vitest";

const app = vi.hoisted(() => ({ base: null as string | null, fail: false, calls: 0 }));
vi.mock("@tauri-apps/api/core", () => ({ convertFileSrc: (p: string, s: string) => `${s}://localhost/${p}` }));
vi.mock("./api", () => ({
  api: {
    getPreviewBase: () => {
      app.calls++;
      return app.fail ? Promise.reject(new Error("no")) : Promise.resolve(app.base);
    },
  },
}));

const SECRET = "a".repeat(64);

beforeEach(() => {
  vi.resetModules();
  Object.assign(app, { base: null, fail: false, calls: 0 });
});

describe("where the player reads a preview from", () => {
  it("the app's own protocol by default (Windows, macOS)", async () => {
    const p = await import("./preview");
    await p.loadPreviewBase();
    expect(p.previewSrc("12")).toBe("velox-preview://localhost/12");
    expect(p.previewSrc("m3")).toBe("velox-preview://localhost/m3");
  });

  it("the private local address when the app gives one (Linux), by id", async () => {
    app.base = `http://127.0.0.1:40123/${SECRET}/`;
    const p = await import("./preview");
    await p.loadPreviewBase();
    expect(p.previewSrc("12")).toBe(`http://127.0.0.1:40123/${SECRET}/12`);
    await p.loadPreviewBase();
    expect(app.calls, "asked once").toBe(1);
  });

  it("never an address that is not this computer's with a full secret", async () => {
    for (const bad of ["http://evil.example/x/", `http://127.0.0.1:1/${SECRET.slice(2)}/`, `https://127.0.0.1:1/${SECRET}/`, `http://127.0.0.1:1/${SECRET}/../`]) {
      vi.resetModules();
      app.base = bad;
      const p = await import("./preview");
      await p.loadPreviewBase();
      expect(p.previewSrc("12"), bad).toBe("velox-preview://localhost/12");
    }
  });

  it("a failed answer keeps the protocol and is asked again next time", async () => {
    app.fail = true;
    const p = await import("./preview");
    await p.loadPreviewBase();
    expect(p.previewSrc("12")).toBe("velox-preview://localhost/12");
    app.fail = false;
    app.base = `http://127.0.0.1:40123/${SECRET}/`;
    await p.loadPreviewBase();
    expect(p.previewSrc("12")).toBe(`http://127.0.0.1:40123/${SECRET}/12`);
  });
});
