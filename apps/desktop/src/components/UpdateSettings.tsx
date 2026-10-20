import { useEffect, useState } from "react";
import { api, normalizeError } from "../api";
import { formatBytes, formatRelative } from "../format";
import { act, store, useStore } from "../hooks";
import { useT, type Key } from "../i18n";
import type { UpdateStatus } from "../types";
import { Panel, ProgressBar } from "../ui/primitives";
import { Rich } from "../ui/rich";

const ERRORS: Record<NonNullable<UpdateStatus["error"]>, Key> = {
  network: "upd.err.network",
  signature: "upd.err.signature",
  malformed: "upd.err.malformed",
  rejected: "upd.err.malformed",
  stale: "upd.err.stale",
  corrupt: "upd.err.corrupt",
  disk: "upd.err.disk",
  install: "upd.err.install",
  off: "upd.unsupported",
};

/** What the updates line says (also the status bar's tooltip). */
export function updateLine(t: ReturnType<typeof useT>, u: UpdateStatus): string {
  if (!u.supported) return t("upd.unsupported");
  switch (u.phase) {
    case "checking":
      return t("upd.checking");
    case "upToDate":
      return t("upd.upToDate");
    case "available":
      return t("upd.available", { v: u.version ?? "", size: formatBytes(u.size ?? 0) });
    case "elsewhere":
      return t("upd.elsewhere", { v: u.version ?? "" });
    case "downloading":
      return u.total > 0
        ? t("upd.downloading", { pct: Math.floor((u.received / u.total) * 100), got: formatBytes(u.received), total: formatBytes(u.total) })
        : t("upd.checking");
    case "ready":
      return t("upd.ready", { v: u.version ?? "" });
    case "failed":
      return t(ERRORS[u.error ?? "network"]);
    default:
      return t("upd.notChecked");
  }
}

/**
 * Settings > Updates. Rust fetches the signed update information, checks it against the publisher's key built into
 * the app, downloads the package and checks its size and checksum: this panel only asks and shows.
 */
export function UpdateSettings() {
  const t = useT();
  const u = useStore((s) => s.update);
  const [busy, setBusy] = useState(false);
  const [opened, setOpened] = useState(false);

  useEffect(() => {
    api.getUpdateStatus().then(
      (update) => store.dispatch({ type: "update", update }),
      () => {},
    );
  }, []);

  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await f();
    } catch (e) {
      store.toast("error", normalizeError(e).message);
    } finally {
      setBusy(false);
    }
  };
  const check = () => run(async () => store.dispatch({ type: "update", update: await api.checkForUpdates() }));
  // One click: download the verified package (unless one is already waiting), then install it. Installing is
  // silent where it can be (the AppImage and the portable copy replace themselves and relaunch); on Windows and
  // the .deb / .rpm it hands off to the system installer, which asks for permission. No file is ever downloaded
  // by hand and no web page is opened. If the download fails, run() shows the error and nothing is installed.
  const updateNow = () =>
    run(async () => {
      let status = u;
      if (status?.phase !== "ready") {
        status = await api.downloadUpdate();
        store.dispatch({ type: "update", update: status });
      }
      if (status?.phase === "ready") {
        const how = await api.installUpdate();
        if (how === "opened") setOpened(true);
      }
    });
  const toggle = async (enabled: boolean) => {
    const next = await act(() => api.setUpdateAuto(enabled), enabled ? t("upd.autoOn") : t("upd.autoOff"));
    if (next) store.dispatch({ type: "update", update: next });
  };

  const tone = !u ? "text-dim" : u.phase === "failed" || u.error ? "text-bad" : u.phase === "available" || u.phase === "ready" ? "text-accent" : "text-dim";
  const working = busy || u?.phase === "checking" || u?.phase === "downloading";
  // A newer version is in hand to install now: available to download (but not the portable "page" kind), or
  // already downloaded and verified. This is when the button is "Update now" instead of "Check now".
  const canUpdateNow = !!u?.supported && ((u.phase === "available" && u.install !== "page") || u.phase === "ready" || u.phase === "downloading");
  return (
    <Panel title={t("upd.title")} hint={t("upd.hint")}>
      <p className="num text-[13px]" data-testid="update-current">
        {t("about.version", { v: u?.current ?? __APP_VERSION__ })}
      </p>
      <p className={`mt-1 text-[13px] ${tone}`} role="status" data-testid="update-status" data-phase={u?.phase ?? ""}>
        {u ? updateLine(t, u) : "…"}
      </p>
      {u?.error && u.phase !== "failed" && (
        <p className="mt-1 text-xs text-bad" data-testid="update-error">
          {t(ERRORS[u.error])}
        </p>
      )}
      {u?.phase === "downloading" && u.total > 0 && (
        <ProgressBar className="mt-2" value={u.received / u.total} label={t("upd.download")} live />
      )}
      {u?.checkedAt != null && u.supported && (
        <p className="mt-1 text-xs text-dim" data-testid="update-checked">
          {t("upd.lastChecked", { when: formatRelative(u.checkedAt) })}
        </p>
      )}
      <div className="mt-3 flex flex-wrap gap-2">
        {/* The button flips: "Check now" until a newer version is found, then a single "Update now" that does the
            whole download-and-install. ("Open the release page" only for the portable copy, which replaces nothing.) */}
        {u?.supported && !canUpdateNow && (
          <button className="btn" data-testid="update-check" disabled={working} onClick={() => void check()}>
            {u.phase === "checking" ? t("upd.checking") : t("upd.checkNow")}
          </button>
        )}
        {canUpdateNow && (
          <button className="btn btn-primary" data-testid="update-now" disabled={working} onClick={() => void updateNow()}>
            {t("upd.updateNow")}
          </button>
        )}
        {u && u.releasePage && (!u.supported || u.phase === "elsewhere" || (u.phase === "available" && u.install === "page")) && (
          <button className="btn" data-testid="update-page" onClick={() => void run(() => api.openReleasePage())}>
            {t("upd.openPage")}
          </button>
        )}
      </div>
      {opened && (
        <p className="mt-2 text-xs text-dim" data-testid="update-opened">
          {t("upd.openedHint")}
        </p>
      )}
      {u?.supported && (
        <label className="mt-3 flex cursor-pointer items-center gap-2 text-[13px]">
          <input type="checkbox" data-testid="update-auto" checked={u.autoCheck} onChange={(e) => void toggle(e.target.checked)} />
          {t("upd.auto")}
        </label>
      )}
      <p className="mt-2 text-xs text-dim">
        <Rich text={t("upd.trust")} />
      </p>
    </Panel>
  );
}
