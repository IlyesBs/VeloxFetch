import React from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "@fontsource-variable/cairo/wght.css";
import "./styles.css";
import { applyChrome } from "./chrome";
import { applyLang } from "./i18n";
import { applyTheme, getThemePref } from "./theme";
import { installNativeFeel } from "./nativeFeel";
import { ErrorBoundary } from "./ui/ErrorBoundary";

// Apply the window frame, the saved theme and the language before the first paint, so there is no
// flash of the wrong look or direction.
applyChrome();
applyTheme(getThemePref());
applyLang();

installNativeFeel();

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
);
