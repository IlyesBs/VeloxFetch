import { useCallback, useEffect, useState } from "react";
import { actions } from "../actions";
import { api, normalizeError } from "../api";
import { act, store } from "../hooks";
import type { BrowserIntegration, BrowserName, ExtensionInstall } from "../types";
import { useT, type Key } from "../i18n";
import { Panel } from "../ui/primitives";
import { Rich } from "../ui/rich";

const LABEL: Record<BrowserName, Key> = { chrome: "br.chrome", edge: "br.edge", brave: "br.brave", firefox: "br.firefox" };

/**
 * Browser integration: lets the VeloxFetch extension hand links to this app. On the first start the app sets it
 * up for the default browser by itself (bridge.rs `auto_install_once`). Each browser also has one button:
 * "Install extension" connects that browser, then either asks it to add the published extension from its
 * store (the browser's own prompt) or prepares the extension folder and opens the extensions page for
 * Developer mode. "Disconnect" undoes the registration.
 */
export function BrowserSettings() {
  const t = useT();
  const [state, setState] = useState<BrowserIntegration | null>(null);
  const [busy, setBusy] = useState<BrowserName | null>(null);
  const [installed, setInstalled] = useState<ExtensionInstall | null>(null);

  const refresh = useCallback(async () => {
    try {
      setState(await api.getBrowserIntegration());
    } catch (e) {
      store.toast("error", normalizeError(e).message);
    }
  }, []);
  useEffect(() => void refresh(), [refresh]);

  const install = async (browser: BrowserName) => {
    setBusy(browser);
    const res = await act(() => api.installBrowserExtension(browser));
    if (res) {
      setInstalled(res);
      setState(res.integration);
    }
    setBusy(null);
  };

  const disconnect = async (browser: BrowserName) => {
    setBusy(browser);
    const next = await act(() => api.setBrowserIntegration(browser, false), t("br.disconnected", { browser: t(LABEL[browser]) }));
    if (next) {
      setState(next);
      if (installed?.browser === browser) setInstalled(null);
    }
    setBusy(null);
  };

  const setConfirm = async (enabled: boolean) => {
    await act(() => api.setBrowserConfirm(enabled));
    await refresh();
  };

  const setVideoBar = async (enabled: boolean) => {
    await act(() => api.setVideoBar(enabled), enabled ? t("br.barOn") : t("br.barOff"));
    await refresh();
  };

  const label = (b: BrowserName) => t(LABEL[b]);
  if (!state) {
    return (
      <Panel title={t("br.title")} hint={t("br.hint")}>
        <p className="text-xs text-dim">{t("common.loading")}</p>
      </Panel>
    );
  }

  return (
    <Panel title={t("br.title")} hint={t("br.hint")}>
      <p className="mb-3 text-xs text-dim" data-testid="extension-intro">
        <Rich text={t("br.intro")} />
      </p>

      <ul className="space-y-2" data-testid="browser-list">
        {state.browsers.map((b) => (
          <li key={b.browser} className="flex items-center gap-3" data-testid={`browser-${b.browser}`}>
            <span className="min-w-0 flex-1">
              <span className="text-[13px] font-medium">{label(b.browser)}</span>
              {b.isDefault && <span className="ms-2 rounded-full bg-accent-soft px-2 py-0.5 text-[11px] text-accent">{t("br.default")}</span>}
              {b.supported && !b.found && <span className="ms-2 text-[11px] text-dim">{t("br.notFound")}</span>}
            </span>
            <span className={`text-xs ${b.registered ? "text-ok" : "text-dim"}`} data-testid={`browser-${b.browser}-status`}>
              {!b.supported ? t("br.unsupported") : b.registered ? t("br.connected") : t("br.notConnected")}
            </span>
            {b.registered && (
              <button className="btn btn-ghost" data-testid={`browser-${b.browser}-steps`} disabled={busy !== null} onClick={() => void install(b.browser)}>
                {t("br.steps")}
              </button>
            )}
            <button
              className={b.registered ? "btn" : "btn btn-primary"}
              data-testid={`browser-${b.browser}-toggle`}
              disabled={!b.supported || !state.hostPresent || busy !== null}
              onClick={() => void (b.registered ? disconnect(b.browser) : install(b.browser))}
            >
              {busy === b.browser ? t("common.working") : b.registered ? t("br.disconnect") : t("br.install")}
            </button>
          </li>
        ))}
      </ul>

      <label className="mt-4 flex cursor-pointer items-center gap-2 text-[13px]">
        <input type="checkbox" data-testid="browser-confirm-toggle" checked={state.confirmDownloads} onChange={(e) => void setConfirm(e.target.checked)} />
        {t("br.confirm")}
      </label>
      <label className="mt-2 flex cursor-pointer items-center gap-2 text-[13px]">
        <input type="checkbox" data-testid="video-bar-toggle" checked={state.videoBar} onChange={(e) => void setVideoBar(e.target.checked)} />
        <span>
          <Rich text={t("br.bar")} />
        </span>
      </label>
      <p className="mt-1 ps-6 text-xs text-dim">{t("br.barDelay")}</p>

      {installed && (
        <ol className="selectable mt-4 list-decimal space-y-1.5 rounded-xl border border-line bg-raised p-3.5 ps-8 text-xs" data-testid="extension-steps">
          {installed.method === "store" ? (
            <>
              <li>{t("br.stStore", { browser: label(installed.browser) })}</li>
              <li>
                <Rich text={t("br.stStoreAccept")} />
              </li>
            </>
          ) : (
            <>
              <li>
                {installed.openedBrowser
                  ? t("br.stConnected", { browser: label(installed.browser) })
                  : t("br.stConnectedOpenYourself", { browser: label(installed.browser) })}
              </li>
              <li>
                <Rich text={installed.browser === "firefox" ? t("br.stFirefox") : t("br.stDev")} />
              </li>
              <li>
                {installed.openedFolder ? t("br.stFolderOpened") : t("br.stFolder")}{" "}
                <code className="break-all rounded bg-canvas px-1.5 py-0.5" data-testid="extension-folder">
                  {installed.folder}
                </code>{" "}
                <button className="btn btn-ghost h-6 px-2 text-xs" onClick={() => void actions.copy(installed.folder)}>
                  {t("common.copyPath")}
                </button>
              </li>
            </>
          )}
          <li>
            <Rich text={t("br.stDone")} />
          </li>
        </ol>
      )}

      {!state.hostPresent && (
        <p className="mt-3 text-xs text-bad" role="alert" data-testid="browser-host-missing">
          {t("br.hostMissing")}
        </p>
      )}
      {!state.linkRunning && (
        <p className="mt-3 text-xs text-warn" role="status" data-testid="browser-link-down">
          {t("br.linkDown")}
        </p>
      )}

      {state.secretsProtected ? (
        <p className="mt-3 text-xs text-dim" data-testid="secrets-protected">
          {t("br.secretsOk")}
        </p>
      ) : (
        <p className="mt-3 text-xs text-warn" role="status" data-testid="secrets-memory-only">
          {t("br.secretsMem")}
        </p>
      )}

      <p className="mt-3 text-xs text-dim">{t("br.footer")}</p>
    </Panel>
  );
}
