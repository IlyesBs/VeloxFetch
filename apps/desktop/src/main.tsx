import React from "react";
import { createRoot } from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import PromptApp from "./PromptApp";
import "@fontsource-variable/cairo/wght.css";
import "./styles.css";
import { applyChrome } from "./chrome";
import { applyLang } from "./i18n";
import { applyTheme, getThemePref } from "./theme";
import { installNativeFeel } from "./nativeFeel";
import { installSelectMenus } from "./ui/selectMenu";
import { ErrorBoundary } from "./ui/ErrorBoundary";

// Apply the window frame, the saved theme and the language before the first paint, so there is no
// flash of the wrong look or direction.
applyChrome();
applyTheme(getThemePref());
applyLang();

installNativeFeel();
// Drop-down lists in the theme's colours on every system (the system list ignores the page's colours).
installSelectMenus();

/** The download popup (lib.rs `show_prompt`) loads the same page in its own window. */
function isPrompt(): boolean {
  try {
    return getCurrentWindow().label === "download-prompt";
  } catch {
    return false;
  }
}

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      {isPrompt() ? <PromptApp /> : <App />}
    </ErrorBoundary>
  </React.StrictMode>,
);
