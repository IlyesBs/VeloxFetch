// Download Profiles, UI side: names, descriptions and the lines that say what a profile sets and what is in force.
// Pure functions (no React, no network). The rules live in Rust (crates/download-manager/src/profiles.rs): a profile
// holds only what it changes; everything else keeps the user's own setting.

import { formatSpeed } from "./format";
import type { Key } from "./i18n";
import type { BuiltInProfile, EffectiveSettings, ProfileInfo, ProfileSettings, Profiles } from "./types";

export type Line = { key: Key; vars?: Record<string, string | number> };

const BUILT_IN_NAME: Record<BuiltInProfile, Key> = {
  default: "prof.default",
  fast: "prof.fast",
  balanced: "prof.balanced",
  lowResource: "prof.lowResource",
};

const BUILT_IN_HINT: Record<BuiltInProfile, Key> = {
  default: "prof.defaultHint",
  fast: "prof.fastHint",
  balanced: "prof.balancedHint",
  lowResource: "prof.lowResourceHint",
};

const isBuiltIn = (id: string): id is BuiltInProfile => id in BUILT_IN_NAME;

/** The name shown: a built-in profile in the user's language, a custom one as the user typed it. */
export function profileName(p: ProfileInfo, t: (k: Key) => string): string {
  return p.builtIn && isBuiltIn(p.id) ? t(BUILT_IN_NAME[p.id]) : (p.name ?? p.id);
}

/** What a built-in profile is for (custom profiles: none). */
export const profileHint = (p: ProfileInfo): Key | null => (p.builtIn && isBuiltIn(p.id) ? BUILT_IN_HINT[p.id] : null);

/** What the profile sets, one line per setting it changes ("keeps your settings" when it changes none). */
export function profileLines(s: ProfileSettings): Line[] {
  const out: Line[] = [];
  if (s.speedLimitBps != null) out.push(s.speedLimitBps === 0 ? { key: "prof.set.noLimit" } : { key: "prof.set.limit", vars: { speed: formatSpeed(s.speedLimitBps) } });
  if (s.maxActive != null) out.push({ key: "prof.set.active", vars: { n: s.maxActive } });
  if (s.maxConnections != null) out.push({ key: "prof.set.connections", vars: { n: s.maxConnections } });
  if (s.adaptive != null) out.push({ key: s.adaptive ? "prof.set.adaptiveOn" : "prof.set.adaptiveOff" });
  return out.length ? out : [{ key: "prof.set.none" }];
}

/** The values in force, for the summary line. */
export function effectiveLines(e: EffectiveSettings): Line[] {
  return [
    e.speedLimitBps === null ? { key: "prof.eff.noLimit" } : { key: "prof.eff.limit", vars: { speed: formatSpeed(e.speedLimitBps) } },
    { key: "prof.eff.active", vars: { n: e.maxActive } },
    { key: "prof.eff.connections", vars: { n: e.maxConnections } },
    { key: !e.smartOn ? "prof.eff.smartOff" : e.adaptive ? "prof.eff.smartAuto" : "prof.eff.smartOn" },
  ];
}

export const activeProfile = (p: Profiles | null): ProfileInfo | null => p?.profiles.find((x) => x.id === p.active) ?? null;

/** Which of the user's own settings the active profile replaces right now (for notes beside those settings). */
export function overridden(p: Profiles | null): { speed: boolean; active: boolean; connections: boolean; adaptive: boolean } {
  const s = activeProfile(p)?.settings ?? {};
  return { speed: s.speedLimitBps != null, active: s.maxActive != null, connections: s.maxConnections != null, adaptive: s.adaptive != null };
}

/** A name for a copy that no custom profile has yet ("Fast (copy)", "Fast (copy 2)"...). */
export function copyName(base: string, taken: string[], t: (k: Key, v?: Record<string, string | number>) => string): string {
  const lower = new Set(taken.map((n) => n.toLowerCase()));
  for (let i = 1; i < 100; i++) {
    const name = (i === 1 ? t("prof.copyOf", { name: base }) : t("prof.copyN", { name: base, n: i })).slice(0, 40);
    if (!lower.has(name.toLowerCase())) return name;
  }
  return base.slice(0, 40);
}
