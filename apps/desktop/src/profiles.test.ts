import { describe, expect, it } from "vitest";
import en from "./locales/en.json";
import fr from "./locales/fr.json";
import ar from "./locales/ar.json";
import { activeProfile, copyName, effectiveLines, overridden, profileHint, profileLines, profileName } from "./profiles";
import type { EffectiveSettings, ProfileInfo, Profiles } from "./types";

const t = (k: string, v?: Record<string, string | number>) =>
  Object.entries(v ?? {}).reduce((s, [a, b]) => s.replace(`{${a}}`, String(b)), (en as unknown as Record<string, string>)[k] ?? k);

const BUILT: ProfileInfo[] = [
  { id: "default", name: null, builtIn: true, settings: {} },
  { id: "fast", name: null, builtIn: true, settings: { maxConnections: 32, adaptive: false } },
  { id: "balanced", name: null, builtIn: true, settings: { adaptive: true } },
  { id: "lowResource", name: null, builtIn: true, settings: { maxActive: 1, maxConnections: 4 } },
];
const BASE: EffectiveSettings = { speedLimitBps: null, maxActive: 3, maxConnections: 16, smartOn: false, adaptive: false };

describe("download profiles in the interface", () => {
  it("names built-in profiles in the user's language and custom ones as typed", () => {
    expect(BUILT.map((p) => profileName(p, t))).toEqual(["Default", "Fast", "Balanced", "Low resource"]);
    expect(profileName({ id: "c1", name: "Night", builtIn: false, settings: {} }, t)).toBe("Night");
    expect(profileHint(BUILT[1]!)).toBe("prof.fastHint");
    expect(profileHint({ id: "c1", name: "x", builtIn: false, settings: {} })).toBeNull();
  });

  it("says only what a profile changes", () => {
    expect(profileLines({}).map((l) => l.key)).toEqual(["prof.set.none"]);
    expect(profileLines(BUILT[3]!.settings).map((l) => t(l.key, l.vars))).toEqual(["1 at once", "Up to 4 connections"]);
    expect(profileLines({ speedLimitBps: 0 }).map((l) => l.key)).toEqual(["prof.set.noLimit"]);
    expect(profileLines({ speedLimitBps: 1024 * 1024 })[0]!.key).toBe("prof.set.limit");
    expect(profileLines({ adaptive: false }).map((l) => l.key)).toEqual(["prof.set.adaptiveOff"]);
  });

  it("shows what is in force and which of the user's settings the profile replaces", () => {
    expect(effectiveLines(BASE).map((l) => l.key)).toEqual(["prof.eff.noLimit", "prof.eff.active", "prof.eff.connections", "prof.eff.smartOff"]);
    expect(effectiveLines({ ...BASE, smartOn: true, adaptive: true })[3]!.key).toBe("prof.eff.smartAuto");
    const p: Profiles = { active: "lowResource", profiles: BUILT, base: BASE, effective: { ...BASE, maxActive: 1, maxConnections: 4 } };
    expect(activeProfile(p)?.id).toBe("lowResource");
    expect(overridden(p)).toEqual({ speed: false, active: true, connections: true, adaptive: false });
    expect(overridden(null)).toEqual({ speed: false, active: false, connections: false, adaptive: false });
  });

  it("finds a free name for a copy", () => {
    expect(copyName("Fast", [], t)).toBe("Fast (copy)");
    expect(copyName("Fast", ["fast (COPY)"], t)).toBe("Fast (copy 2)");
    expect(copyName("x".repeat(60), [], t).length).toBeLessThanOrEqual(40);
  });

  it("has every sentence in every language", () => {
    const keys = new Set<string>();
    for (const p of BUILT) {
      keys.add(BUILT.indexOf(p) >= 0 ? `prof.${p.id}` : "");
      keys.add(profileHint(p)!);
      for (const l of profileLines(p.settings)) keys.add(l.key);
    }
    for (const s of [{ speedLimitBps: 0 }, { speedLimitBps: 5 }, { adaptive: true }, {}]) for (const l of profileLines(s)) keys.add(l.key);
    for (const e of [BASE, { ...BASE, speedLimitBps: 9, smartOn: true }, { ...BASE, smartOn: true, adaptive: true }]) for (const l of effectiveLines(e)) keys.add(l.key);
    for (const file of [en, fr, ar] as unknown as Record<string, string>[]) expect([...keys].filter((k) => !(k in file))).toEqual([]);
  });
});
