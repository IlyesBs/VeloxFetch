import React from "react";
import { createRoot } from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import PromptApp from "./PromptApp";
import IndicatorApp, { applyIndicatorTheme } from "./IndicatorApp";
import "@fontsource-variable/cairo/wght.css";
import "./styles.css";
import { applyChrome } from "./chrome";
import { applyLang, getLang, loadLang } from "./i18n";
import { applyTheme, getThemePref } from "./theme";
import { applyUiFont, learnUiFont } from "./uiFont";
import { installNativeFeel } from "./nativeFeel";
import { installSelectMenus } from "./ui/selectMenu";
import { ErrorBoundary } from "./ui/ErrorBoundary";

// Apply the window frame, the saved theme and the language before the first paint, so there is no
// flash of the wrong look or direction.
/** The window this page is in: the main window, the download popup (lib.rs `show_prompt`) or the download
 *  indicator (indicator.rs), which load the same page. */
function label(): string {
  try {
    return getCurrentWindow().label;
  } catch {
    return "main";
  }
}
const windowLabel = label();

applyChrome();
// The download indicator has no frame to colour: only the page follows the theme.
if (windowLabel === "download-indicator") applyIndicatorTheme(getThemePref());
else applyTheme(getThemePref());
applyLang();
// Linux: straight to the interface font this computer has (the first picture waits on font lookups).
applyUiFont();

installNativeFeel();
// The saved language's texts (English is built in), before the first render.
await loadLang(getLang());

// Drop-down lists in the theme's colours on every system (the system list ignores the page's colours).
installSelectMenus();

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      {windowLabel === "download-prompt" ? <PromptApp /> : windowLabel === "download-indicator" ? <IndicatorApp /> : <App />}
    </ErrorBoundary>
  </React.StrictMode>,
);

// Linux: find that font once the window is on screen, for the next start.
learnUiFont();
