import { describe, expect, it } from "vitest";
import type { FolderName, Storage } from "./types";

// The component module reads <html> through i18n when it loads: a bare stand-in is enough here.
(globalThis as unknown as { document: unknown }).document = { documentElement: {} };
const { KIND_LABEL, rememberedFor } = await import("./components/RememberFolder");

const folder = (kind: FolderName["kind"], remembered: string | null, rememberedAvailable = true): FolderName => ({
  kind,
  folder: kind,
  defaultFolder: kind,
  remembered,
  rememberedAvailable,
});
const storage = (folders: FolderName[]): Storage => ({ root: "C:\\Users\\me\\Downloads", defaultRoot: "", isDefault: true, organize: false, folders });

describe("the folder remembered for each kind of file", () => {
  const s = storage([folder("video", "D:\\My Videos"), folder("document", null), folder("archive", "E:\\Archives", false)]);

  it("is the one of that kind only", () => {
    expect(rememberedFor(s, "video")).toEqual({ path: "D:\\My Videos", available: true });
    expect(rememberedFor(s, "document")).toBeNull();
    expect(rememberedFor(s, "program")).toBeNull();
  });

  it("says when it cannot be used right now (the usual folder is used meanwhile)", () => {
    expect(rememberedFor(s, "archive")).toEqual({ path: "E:\\Archives", available: false });
  });

  it("is nothing while the kind or the settings are not known yet", () => {
    expect(rememberedFor(s, null)).toBeNull();
    expect(rememberedFor(null, "video")).toBeNull();
  });

  it("names every kind the app sorts", () => {
    expect(Object.keys(KIND_LABEL).sort()).toEqual(["archive", "audio", "document", "image", "other", "program", "video"]);
  });
});
