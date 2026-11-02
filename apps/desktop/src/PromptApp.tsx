// The download popup: a small window of its own that asks about a download the browser handed over (the
// confirmation, or the video dialog for a video), while the main window stays as it is (in the tray, minimised or
// open). Rust opens it (lib.rs `show_prompt`), shows it once its page has loaded, and closes it when nothing waits.
// Closing it answers "no" to the download it shows, like Cancel; the next one waiting takes its place.
import { useEffect } from "react";
import { api, normalizeError, onManagerEvent } from "./api";
import { ownWindowButtons, windowControl } from "./chrome";
import { PendingDialog } from "./components/PendingDialog";
import { store, useStore } from "./hooks";
import { useT } from "./i18n";
import { Toasts } from "./ui/primitives";
import type { PendingDownload } from "./types";

/** The popup asks about what the browser handed over (the app's own are asked about in the main window). */
const fromBrowser = (p: PendingDownload) => !p.fromApp;

/** Only what the popup shows is kept: the downloads waiting for an answer. */
async function follow(): Promise<() => void> {
  const unlisten = await onManagerEvent((ev) => {
    if (ev.type === "pending" || ev.type === "pendingRemoved") store.dispatch(ev);
  });
  const sync = async () => {
    try {
      const now = await api.listPendingDownloads();
      for (const p of store.getState().pending) {
        if (!now.some((n) => n.token === p.token)) store.dispatch({ type: "pendingRemoved", token: p.token });
      }
      for (const p of now) store.dispatch({ type: "pending", pending: p });
    } catch (e) {
      store.toast("error", normalizeError(e).message);
    }
  };
  await sync();
  // Events are best-effort: catch up when the window comes back to the front.
  window.addEventListener("focus", sync);
  return () => {
    unlisten();
    window.removeEventListener("focus", sync);
  };
}

/** The window takes the height of the dialog it shows: small while a link is looked up, taller once its details
 *  are there, never empty space under the buttons (Rust keeps it within the screen; past that the dialog scrolls). */
function useFitWindow() {
  useEffect(() => {
    let frame = 0;
    let last = 0;
    let watched: Element[] = [];
    const sizes = new ResizeObserver(() => schedule());
    const measure = () => {
      frame = 0;
      const dialog = document.querySelector<HTMLElement>(".prompt-window [role='dialog']");
      const bar = document.querySelector<HTMLElement>(".prompt-titlebar");
      if (!dialog || !bar) return;
      // What is inside can change size without the dialog's own box changing (it scrolls past the screen's height).
      const now = [dialog, ...Array.from(dialog.children)];
      if (now.length !== watched.length || now.some((el, i) => el !== watched[i])) {
        sizes.disconnect();
        for (const el of now) sizes.observe(el);
        watched = now;
      }
      const height = Math.ceil(bar.offsetHeight + dialog.scrollHeight);
      if (Math.abs(height - last) < 2) return;
      last = height;
      void windowControl("fit", height);
    };
    const schedule = () => {
      if (!frame) frame = requestAnimationFrame(measure);
    };
    const changes = new MutationObserver(schedule);
    changes.observe(document.body, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ["class", "open", "hidden"] });
    schedule();
    return () => {
      cancelAnimationFrame(frame);
      changes.disconnect();
      sizes.disconnect();
    };
  }, []);
}

function PromptTitleBar() {
  const t = useT();
  const first = useStore((s) => s.pending.find(fromBrowser));
  // Same as Cancel for the one shown (Rust does the same when the system closes the window).
  const close = () => (first ? void api.dismissPendingDownload(first.token).catch(() => {}) : void windowControl("close"));
  return (
    <header className="prompt-titlebar titlebar relative z-50 flex shrink-0 items-center gap-2.5 ps-4" data-tauri-drag-region data-testid="prompt-titlebar">
      <span className="brand-name truncate text-[15px]" data-tauri-drag-region>
        VeloxFetch
      </span>
      <div className="h-full min-w-4 flex-1" data-tauri-drag-region />
      {ownWindowButtons() && (
        <div className="winbtns flex h-full items-center">
          <button className="winbtn" aria-label={t("win.minimize")} title={t("win.minimize")} onClick={() => void windowControl("minimize")}>
            <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1" shapeRendering="crispEdges">
              <path d="M0 5.5h10" />
            </svg>
          </button>
          <button className="winbtn winbtn-close" aria-label={t("win.close")} title={t("win.close")} data-testid="prompt-close" onClick={close}>
            <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.1">
              <path d="M.5.5l9 9M9.5.5l-9 9" />
            </svg>
          </button>
        </div>
      )}
    </header>
  );
}

export default function PromptApp() {
  useFitWindow();
  useEffect(() => {
    let cleanup: (() => void) | undefined;
    let cancelled = false;
    follow()
      .then((c) => (cancelled ? c() : (cleanup = c)))
      .catch((e) => store.toast("error", normalizeError(e).message));
    return () => {
      cancelled = true;
      cleanup?.();
    };
  }, []);
  return (
    <div className="prompt-window relative flex h-full flex-col" data-testid="prompt-window">
      <PromptTitleBar />
      <PendingDialog which={fromBrowser} />
      <Toasts />
    </div>
  );
}
