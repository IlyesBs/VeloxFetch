import { getLang, subscribe as onLangChange, t } from "./i18n";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { api, normalizeError } from "./api";
import { FILL_LINK, OPEN_ADD } from "./components/AddDownload";
import { BatchDialog } from "./components/BatchDialog";
import { ClipboardBanner } from "./components/ClipboardBanner";
import { ExtensionBanner } from "./components/ExtensionBanner";
import { MediaDialog } from "./components/MediaDialog";
import { PendingDialog } from "./components/PendingDialog";
import { NewLinkHost } from "./components/NewLinkDialog";
import { GameModeBanner, toggleGameMode } from "./components/GameMode";
import { PowerBanner } from "./components/PowerBanner";
import { ToolsUpdateBanner } from "./components/ToolsUpdateBanner";
import { Sidebar, type NavTarget, type RouteName } from "./components/shell/Sidebar";
import { StatusBar } from "./components/shell/StatusBar";
import { TitleBar } from "./components/shell/TitleBar";
import { bootstrap, store } from "./hooks";
import { showWindowWhenReady } from "./nativeFeel";
import { paintAurora } from "./aurora";
import { onThemeChange } from "./theme";
import { useMedia, usePrefs } from "./prefs";
import { watchThrottle } from "./throttle";
import { Toasts } from "./ui/primitives";
import { ActiveView } from "./views/ActiveView";
import { CompletedView } from "./views/CompletedView";
import { DetailView } from "./views/DetailView";
import { MediaDetailView } from "./views/MediaDetailView";
import { OPEN_MEDIA } from "./components/MediaJobs";
import { DownloadsView, FOCUS_SEARCH, type Filter } from "./views/DownloadsView";
import { HistoryView } from "./views/HistoryView";
import { QueueView } from "./views/QueueView";
import { SettingsView } from "./views/SettingsView";

type Route = { name: RouteName } | { name: "detail"; id: number } | { name: "media"; id: number };

/** http(s) links in some text (a paste, a drop), in order, without repeats; at most 1000. */
export function linksIn(text: string): string[] {
  const out: string[] = [];
  for (const word of text.split(/\s+/)) {
    const w = word.trim().replace(/^[<("']+|[>)"',;.]+$/g, "");
    if (!/^https?:\/\//i.test(w)) continue;
    try {
      new URL(w);
    } catch {
      continue;
    }
    if (!out.includes(w)) out.push(w);
    if (out.length >= 1000) break;
  }
  return out;
}

const isEditable = (t: EventTarget | null) => t instanceof Element && !!t.closest("input, textarea, select, [contenteditable='true']");
/** A modal (dialog element or our own overlays) is open: shortcuts must not act behind it. */
const modalOpen = () => !!document.querySelector("dialog[open], [aria-modal='true']");

export default function App() {
  const [route, setRoute] = useState<Route>({ name: "downloads" });
  /** The batch dialog: closed (null) or open, optionally with links to start from. */
  const [batch, setBatch] = useState<{ links: string } | null>(null);
  /** The video-stream dialog (HLS / DASH): closed (null) or open, optionally with a link to look at. */
  const [stream, setStream] = useState<{ url: string } | null>(null);
  const [dropping, setDropping] = useState(false);
  /** What the Downloads list shows (its tabs). */
  const [filter, setFilter] = useState<Filter>("all");
  // The sidebar folds to an icon rail on narrow windows, or when the user folds it.
  const wide = useMedia("(min-width: 900px)");
  const { sideFolded } = usePrefs();
  const folded = !wide || sideFolded;
  const routeRef = useRef(route);
  routeRef.current = route;

  // The tray menu and notifications (shown by Rust) follow the interface language.
  useEffect(() => {
    const sync = () => void api.setDesktopPrefs({ language: getLang() }).catch(() => undefined);
    sync();
    return onLangChange(sync);
  }, []);

  // The backdrop as a picture (see aurora.ts).
  const auroraRef = useRef<HTMLDivElement>(null);
  useEffect(() => (auroraRef.current ? paintAurora(auroraRef.current, onThemeChange) : undefined), []);

  // The first render is on the page: show the (hidden) window now, at commit, before the data arrives (a
  // passive effect runs later, held back while the window is hidden; see showWindowWhenReady).
  useLayoutEffect(() => showWindowWhenReady(), []);

  useEffect(() => {
    let cleanup: (() => void) | undefined;
    let cancelled = false;
    bootstrap()
      .then((c) => (cancelled ? c() : (cleanup = c)))
      .catch((e) => store.toast("error", normalizeError(e).message));
    const stopThrottle = watchThrottle();
    return () => {
      cancelled = true;
      cleanup?.();
      stopThrottle();
    };
  }, []);

  const open = useCallback((id: number) => setRoute({ name: "detail", id }), []);
  const home = useCallback(() => setRoute({ name: "downloads" }), []);
  // every sidebar row has its own view (Downloads is the dashboard, Active the work in progress, Completed the files)
  const nav = useCallback((target: NavTarget) => setRoute({ name: target }), []);
  // A video download's details page counts as a details page (the sidebar lights Downloads).
  const current: NavTarget | "detail" = route.name === "media" ? "detail" : route.name;
  useEffect(() => {
    const onMedia = (e: Event) => {
      const id = (e as CustomEvent<number>).detail;
      if (typeof id === "number") setRoute({ name: "media", id });
    };
    window.addEventListener(OPEN_MEDIA, onMedia);
    return () => window.removeEventListener(OPEN_MEDIA, onMedia);
  }, []);

  /** Go to the Downloads view, then send it an event once it is mounted. */
  const toDownloads = useCallback((event: string, detail?: string) => {
    const send = () => window.dispatchEvent(new CustomEvent(event, { detail }));
    if (routeRef.current.name === "downloads") return send();
    setRoute({ name: "downloads" });
    setTimeout(send, 80);
  }, []);

  /** Links from a paste or a drop: one goes in the link bar (the user still presses Add), several open "Add several". */
  const takeLinks = useCallback(
    (text: string) => {
      const links = linksIn(text);
      if (links.length === 0) return false;
      if (links.length === 1) toDownloads(FILL_LINK, links[0]);
      else setBatch({ links: links.join("\n") });
      return true;
    },
    [toDownloads],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey;
      if (!mod || e.altKey || modalOpen()) return;
      const k = e.key.toLowerCase();
      const go = (fn: () => void) => {
        e.preventDefault();
        fn();
      };
      if (k === "n" && e.shiftKey) go(() => setBatch({ links: "" }));
      else if (k === "n") go(() => toDownloads(OPEN_ADD));
      else if (k === "l") go(() => toDownloads(FILL_LINK, ""));
      else if (k === "f") go(() => (["history", "completed"].includes(routeRef.current.name) ? window.dispatchEvent(new Event(FOCUS_SEARCH)) : toDownloads(FOCUS_SEARCH)));
      else if (k === "1") go(() => nav("downloads"));
      else if (k === "2") go(() => setRoute({ name: "queue" }));
      else if (k === "3") go(() => setRoute({ name: "history" }));
      else if (k === ",") go(() => setRoute({ name: "settings" }));
      else if (k === "g" && !e.shiftKey) go(() => toggleGameMode(t));
    };
    const onPaste = (e: ClipboardEvent) => {
      if (isEditable(e.target) || modalOpen()) return;
      if (takeLinks(e.clipboardData?.getData("text") ?? "")) e.preventDefault();
    };
    const hasText = (e: DragEvent) => !!e.dataTransfer && [...e.dataTransfer.types].some((t) => t === "text/uri-list" || t === "text/plain");
    const onOver = (e: DragEvent) => {
      if (!hasText(e) || modalOpen()) return;
      e.preventDefault();
      if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
      setDropping(true);
    };
    const onLeave = (e: DragEvent) => {
      if (!e.relatedTarget) setDropping(false);
    };
    const onDrop = (e: DragEvent) => {
      setDropping(false);
      if (!hasText(e) || modalOpen() || isEditable(e.target)) return;
      e.preventDefault();
      const text = e.dataTransfer?.getData("text/uri-list") || e.dataTransfer?.getData("text/plain") || "";
      // uri-list: one link per line, lines starting with # are comments.
      if (!takeLinks(text.split(/\r?\n/).filter((l) => !l.startsWith("#")).join("\n"))) store.toast("error", t("app.notLink"));
    };
    document.addEventListener("keydown", onKey);
    document.addEventListener("paste", onPaste);
    document.addEventListener("dragover", onOver);
    document.addEventListener("dragleave", onLeave);
    document.addEventListener("drop", onDrop);
    return () => {
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("paste", onPaste);
      document.removeEventListener("dragover", onOver);
      document.removeEventListener("dragleave", onLeave);
      document.removeEventListener("drop", onDrop);
    };
  }, [takeLinks, toDownloads, nav]);

  return (
    <div className={`relative flex h-full flex-col ${dropping ? "drop-target" : ""}`}>
      <div className="aurora" aria-hidden="true" ref={auroraRef} />
      <div className="flex min-h-0 flex-1">
        <Sidebar current={current} folded={folded} canUnfold={wide} onNav={nav} />
        <div className="flex min-w-0 flex-1 flex-col overflow-hidden">
          <TitleBar route={current} />
          <PowerBanner />
          <GameModeBanner />
          <ExtensionBanner />
          <ToolsUpdateBanner />
          <ClipboardBanner onReviewMany={(links) => setBatch({ links: links.join("\n") })} onStream={(url) => setStream({ url })} />
          {/* Never scrolls sideways: right to left, the Linux web view puts the bar on the left but lays the content out as
              if it were on the right, one bar too wide, which showed a stray sideways scrollbar (the side padding covers
              the overlap, so nothing is hidden). */}
          <main className="flex-1 overflow-y-auto overflow-x-hidden" data-testid="main">
            <div className={`mx-auto px-4 pb-6 pt-1 sm:px-6 ${route.name === "downloads" ? "max-w-[1720px]" : route.name === "active" ? "max-w-[1400px]" : "max-w-6xl"}`}>
              {route.name === "downloads" && (
                <DownloadsView
                  onOpen={open}
                  onHistory={() => setRoute({ name: "history" })}
                  onBatch={() => setBatch({ links: "" })}
                  onStream={(url) => setStream({ url })}
                  onQueue={() => setRoute({ name: "queue" })}
                  onSettings={() => setRoute({ name: "settings" })}
                  filter={filter}
                  onFilter={setFilter}
                  showHud={folded}
                />
              )}
              {route.name === "active" && <ActiveView onOpen={open} onQueue={() => setRoute({ name: "queue" })} onAdd={() => toDownloads(OPEN_ADD)} />}
              {route.name === "completed" && <CompletedView onOpen={open} />}
              {route.name === "queue" && <QueueView onOpen={open} />}
              {route.name === "history" && <HistoryView />}
              {route.name === "settings" && <SettingsView />}
              {route.name === "detail" && <DetailView key={route.id} id={route.id} onBack={home} />}
              {route.name === "media" && <MediaDetailView key={route.id} id={route.id} onBack={home} />}
            </div>
          </main>
        </div>
      </div>
      <StatusBar
        onUpdates={() => {
          setRoute({ name: "settings" });
          // after the settings page has rendered
          setTimeout(() => document.getElementById("set-updates")?.scrollIntoView({ behavior: "smooth", block: "start" }), 60);
        }}
      />
      {batch && <BatchDialog initialLinks={batch.links} onClose={() => setBatch(null)} />}
      {stream && <MediaDialog initialUrl={stream.url} onClose={() => setStream(null)} />}
      <PendingDialog />
      <NewLinkHost />
      <Toasts />
    </div>
  );
}
