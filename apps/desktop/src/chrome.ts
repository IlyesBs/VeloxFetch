// The window frame VeloxFetch runs in, as told by Rust (lib.rs `window_chrome`, a frozen constant set
// before the page loads). Put on <html> as data-platform / data-surface, which the CSS keys off:
//   mica  - Windows 11, the page is see-through and Mica tints it
//   glass - macOS, native traffic lights over our own title bar
//   solid - Windows 10 / Linux, an opaque window
// Outside Tauri (unit tests, a plain browser) it is guessed from the user agent, always "solid".

import { api, type WindowAction } from "./api";

export type Platform = "windows" | "macos" | "linux";
export type Surface = "mica" | "glass" | "solid";

interface Chrome {
  platform: Platform;
  surface: Surface;
}

function guess(): Chrome {
  const ua = typeof navigator === "undefined" ? "" : navigator.userAgent;
  const platform: Platform = /Mac/i.test(ua) ? "macos" : /Windows/i.test(ua) ? "windows" : "linux";
  return { platform, surface: "solid" };
}

export function chrome(): Chrome {
  const c = (window as unknown as { __VELOX_CHROME__?: Partial<Chrome> }).__VELOX_CHROME__;
  const g = guess();
  const platform = c?.platform === "windows" || c?.platform === "macos" || c?.platform === "linux" ? c.platform : g.platform;
  const surface = c?.surface === "mica" || c?.surface === "glass" || c?.surface === "solid" ? c.surface : g.surface;
  return { platform, surface };
}

/** Whether the UI draws the minimize / maximize / close buttons itself (everywhere but macOS). */
export const ownWindowButtons = () => chrome().platform !== "macos";

export function applyChrome(): void {
  const { platform, surface } = chrome();
  const root = document.documentElement;
  root.dataset.platform = platform;
  root.dataset.surface = surface;
}

/**
 * Ask Rust to act on this window (title bar buttons, frame theme). Through our own `window_control`
 * command, which only ever touches the calling window; resolves whether it is maximized, or null
 * outside Tauri.
 */
export function windowControl(action: WindowAction): Promise<boolean | null> {
  return api.windowControl(action).catch(() => null);
}

/**
 * Keep the native frame (Mica tint, macOS title bar) in the app's theme, also when the app forces
 * light or dark against the system setting.
 */
export function syncFrameTheme(pref: "system" | "light" | "dark"): void {
  void windowControl(pref === "light" ? "themeLight" : pref === "dark" ? "themeDark" : "themeSystem");
}
