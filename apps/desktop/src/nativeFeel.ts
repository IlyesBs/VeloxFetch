// Make the window behave like a desktop app rather than a web page: no browser context menu
// (Reload / Back / Save as / Copy page URL ...), no reload/print/find/zoom/view-source shortcuts,
// no page text selection or dragging of the UI chrome. Text fields keep the normal edit menu and
// selection; download cards provide their own right-click menu (see DownloadCard).

const isEditable = (t: EventTarget | null): boolean => {
  const el = t instanceof Element ? t : null;
  return !!el?.closest("input, textarea, select, [contenteditable=''], [contenteditable='true']");
};

export function installNativeFeel(): void {
  // Any right-click that is not on a text field or a component with its own menu is swallowed.
  // Components handle their own event first (React listens at the root) and call preventDefault.
  document.addEventListener("contextmenu", (e) => {
    if (!isEditable(e.target)) e.preventDefault();
  });

  document.addEventListener("keydown", (e) => {
    const k = e.key.toLowerCase();
    const mod = e.ctrlKey || e.metaKey;
    const browserKey =
      e.key === "F5" ||
      e.key === "F3" ||
      e.key === "F7" ||
      e.key === "F12" ||
      (mod && ["r", "p", "s", "u", "f", "g", "j", "l", "o"].includes(k)) ||
      (mod && e.shiftKey && ["r", "i", "j", "c"].includes(k)) ||
      (mod && ["+", "-", "=", "0"].includes(e.key)) ||
      (e.altKey && (e.key === "ArrowLeft" || e.key === "ArrowRight"));
    if (browserKey) e.preventDefault();
  });

  // Ctrl + wheel / pinch zoom, and dropping a file on the window (which would navigate to it).
  document.addEventListener("wheel", (e) => e.ctrlKey && e.preventDefault(), { passive: false });
  for (const ev of ["dragover", "drop"]) document.addEventListener(ev, (e) => e.preventDefault());
}

/**
 * The window is created hidden (no blank "loading web page" frame); show it once the UI is on the page.
 * Called from the first effect of <App/>, i.e. after React committed the first render. Not tied to
 * requestAnimationFrame: a page in a hidden window gets no animation frames, so a rAF-based show never ran
 * and the window only appeared through the slow fallback in Rust (as a dark, empty frame on slow machines).
 */
let shown = false;
export function showWindowWhenReady(): void {
  if (shown) return;
  shown = true;
  // Rust shows it (and leaves it in the tray when VeloxFetch was started hidden at login).
  void import("./chrome").then((m) => m.windowControl("ready"));
}
