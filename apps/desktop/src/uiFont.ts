// Linux: start with the interface font this computer has, not a list of names to try.
//
// WebKitGTK asks the system's font matcher about every name of a font list, for each text style on the page, before
// it can draw the first picture. With the full list (Inter, Cantarell, Noto Sans, then the system's) the window
// stayed empty for seconds after it was shown. So the first font of the list that is installed is found once, after
// the window is on screen, kept in localStorage (a per-user cosmetic setting, like the theme), and the next starts
// use only that one (<html data-ui-font>, styles.css). What is drawn is the same font the full list gave. It is
// checked again at each start (quietly, later), so a font installed or removed since is followed. The very first
// time it is found before the first render: a few lookups, against one per text style of the page with the list.

import { chrome } from "./chrome";

const KEY = "velox.ui.font";

/** The interface fonts tried on Linux, in the order of the full list (styles.css `--font-ui`). */
export const LINUX_FONTS = ["Inter", "Cantarell", "Noto Sans"] as const;
export type UiFont = "inter" | "cantarell" | "noto" | "system";

const CODE: Record<(typeof LINUX_FONTS)[number], UiFont> = { Inter: "inter", Cantarell: "cantarell", "Noto Sans": "noto" };

/** The first font of the list that is installed; "system" (the system's interface font) when none is. */
export function pickUiFont(installed: (family: string) => boolean): UiFont {
  for (const f of LINUX_FONTS) if (installed(f)) return CODE[f];
  return "system";
}

const isUiFont = (v: unknown): v is UiFont => v === "inter" || v === "cantarell" || v === "noto" || v === "system";

/** Before the first render: the font found at an earlier start; the first time, found now (a few lookups instead
 *  of one per text style of the page). If that fails, the full list applies. */
export function applyUiFont(): void {
  if (chrome().platform !== "linux") return;
  try {
    const v = localStorage.getItem(KEY);
    document.documentElement.dataset.uiFont = isUiFont(v) ? v : pickUiFont(installed);
  } catch {
    /* the full list */
  }
}

/** Whether `family` is installed: text in it measures differently from the generic fonts it would fall back to. */
function installed(family: string): boolean {
  const ctx = document.createElement("canvas").getContext("2d");
  if (!ctx) return false;
  const sample = "mmmmmmmmmmlli WwQq 0123456789";
  return ["monospace", "serif"].some((generic) => {
    ctx.font = `72px ${generic}`;
    const base = ctx.measureText(sample).width;
    ctx.font = `72px "${family}", ${generic}`;
    return ctx.measureText(sample).width !== base;
  });
}

/** After the window is on screen: find this computer's font and keep it for the next start. */
export function learnUiFont(delayMs = 2500): () => void {
  if (chrome().platform !== "linux") return () => {};
  const timer = window.setTimeout(() => {
    try {
      const font = pickUiFont(installed);
      if (document.documentElement.dataset.uiFont !== font) document.documentElement.dataset.uiFont = font;
      localStorage.setItem(KEY, font);
    } catch {
      /* the full list stays */
    }
  }, delayMs);
  return () => window.clearTimeout(timer);
}
