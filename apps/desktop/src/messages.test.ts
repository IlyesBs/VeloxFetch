import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: () => Promise.reject(new Error("no app")) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));
(globalThis as unknown as { document: unknown }).document = { documentElement: {} };

const { normalizeError } = await import("./api");
const { MESSAGES } = await import("./messages");
const { setLang, t } = await import("./i18n");

afterEach(() => setLang("en"));

function rustSources(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) rustSources(p, out);
    else if (p.endsWith(".rs")) out.push(readFileSync(p, "utf8"));
  }
  return out;
}

describe("messages from Rust in the user's language", () => {
  it("lists every fixed message the manager can answer with", () => {
    const src = rustSources(join(__dirname, "../../../crates/download-manager/src")).join("\n");
    const found = [...src.matchAll(/ManagerError::invalid\("([^"]*)"/g)].map((m) => m[1]!);
    expect(found.length).toBeGreaterThan(50);
    expect(found.filter((m) => !(m in MESSAGES))).toEqual([]);
  });

  it("says a refusal in French and Arabic, and leaves English and unknown messages as they are", () => {
    const err = { code: "invalid_input", message: "invalid input: the file is gone (moved or deleted)" };
    expect(normalizeError(err).message).toBe(err.message);
    setLang("fr");
    expect(normalizeError(err).message).toBe(t("msg.the_file_is_gone_moved_or_deleted"));
    expect(normalizeError({ code: "x", message: "something new" }).message).toBe("something new");
    setLang("ar");
    expect(normalizeError(err).message).not.toMatch(/[a-z]{4}/);
    expect(normalizeError(err).code).toBe("invalid_input");
  });
});
