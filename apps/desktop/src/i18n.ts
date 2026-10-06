// Interface language. The texts live in `locales/<code>.json` (en.json, fr.json, ar.json, ...): every file in that
// folder is picked up when the app is built, so a translation is changed, or a language added, by editing or adding
// a file, without touching the code. Each file has a `_meta` entry: { "name": "Français", "short": "FR", "dir": "ltr" }
// ("rtl" for right-to-left languages). en.json is the reference: a key missing from another file shows in English.
//
// A per-user cosmetic setting kept in localStorage, like the theme. Applied as <html lang dir>, which the CSS keys
// off (fonts, mirrored layout). Components call `useT()` so they re-render when the language changes.

import { useSyncExternalStore } from "react";
import en from "./locales/en.json";

type Meta = { name: string; short: string; dir?: "ltr" | "rtl" };
type File = Record<string, string> & { _meta?: Meta };

export type Key = Exclude<keyof typeof en, "_meta">;
export type Lang = string;

const FILES: Record<string, File> = Object.fromEntries(
  Object.entries(import.meta.glob<File>("./locales/*.json", { eager: true, import: "default" })).map(([p, d]) => [
    p.replace(/^.*\/([^/]+)\.json$/, "$1"),
    d,
  ]),
);

/** The languages there are files for: English first, then by code. */
export const LANGS: { lang: Lang; label: string; short: string; dir: "ltr" | "rtl" }[] = Object.entries(FILES)
  .filter(([, d]) => d._meta?.name)
  .sort(([a], [b]) => (a === "en" ? -1 : b === "en" ? 1 : a.localeCompare(b)))
  .map(([lang, d]) => ({ lang, label: d._meta!.name, short: d._meta!.short || lang.toUpperCase(), dir: d._meta!.dir === "rtl" ? "rtl" : "ltr" }));

const isLang = (l: string | null): l is Lang => !!l && LANGS.some((x) => x.lang === l);
const dirOf = (l: Lang) => LANGS.find((x) => x.lang === l)?.dir ?? "ltr";

/** The saved choice, else English. */
function stored(): Lang {
  try {
    const v = localStorage.getItem(KEY);
    if (isLang(v)) return v;
  } catch {
    /* ignore */
  }
  return "en";
}

const KEY = "velox.lang";

let current: Lang = stored();
const subs = new Set<() => void>();

export function getLang(): Lang {
  return current;
}

/** Put the language on <html>: `lang` (fonts, screen readers) and `dir` (mirrored layout). */
export function applyLang(lang: Lang = current): void {
  const root = document.documentElement;
  root.lang = lang;
  root.dir = dirOf(lang);
}

export function setLang(lang: Lang): void {
  current = lang;
  try {
    localStorage.setItem(KEY, lang);
  } catch {
    /* ignore */
  }
  applyLang(lang);
  subs.forEach((f) => f());
}

/** The text for `key` in the current language, with `{name}` placeholders filled in. */
export function t(key: Key, vars?: Record<string, string | number>): string {
  const text = FILES[current]?.[key] ?? (en as unknown as Record<string, string>)[key] ?? key;
  // In right-to-left languages, each value ("239 GB", "3.7 MB/s", a program name) is a bidi isolate (FSI ... PDI), so its own
  // characters keep their order inside the right-to-left sentence.
  const wrap = dirOf(current) === "rtl" ? (v: string) => `⁨${v}⁩` : (v: string) => v;
  return vars ? text.replace(/\{(\w+)\}/g, (m, k: string) => (k in vars ? wrap(String(vars[k])) : m)) : text;
}

function subscribe(f: () => void) {
  subs.add(f);
  return () => {
    subs.delete(f);
  };
}

/** Re-render on a language change; returns `t`. */
export function useT(): typeof t {
  useSyncExternalStore(subscribe, getLang);
  return t;
}

export function useLang(): Lang {
  return useSyncExternalStore(subscribe, getLang);
}
