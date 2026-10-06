import { useCallback, useEffect, useState } from "react";
import { api, normalizeError } from "../api";
import { act, store } from "../hooks";
import { formatSpeed } from "../format";
import { useT } from "../i18n";
import { throttleStore } from "../throttle";
import type { SmartThrottle } from "../types";
import { Panel } from "../ui/primitives";

const SHARES = [10, 25, 50, 75];

/**
 * Settings > Speed > Smart throttle: slow downloads down while a game or a video call is running, full speed
 * again as soon as it closes. Which programs run is checked on this computer only; nothing is sent anywhere.
 */
export function SmartThrottleSettings() {
  const t = useT();
  const [s, setS] = useState<SmartThrottle | null>(null);
  const [apps, setApps] = useState("");

  const refresh = useCallback(async (resetApps = false) => {
    try {
      const next = await api.getSmartThrottle();
      setS(next);
      throttleStore.set(next);
      if (resetApps) setApps(next.apps.join("\n"));
    } catch (e) {
      store.toast("error", normalizeError(e).message);
    }
  }, []);

  useEffect(() => {
    void refresh(true);
    // The status line follows the programs that start and stop.
    const timer = setInterval(() => document.visibilityState === "visible" && void refresh(), 3000);
    return () => clearInterval(timer);
  }, [refresh]);

  const save = async (patch: { enabled?: boolean; apps?: string[]; sharePercent?: number }, done?: string) => {
    const next = await act(() => api.setSmartThrottle(patch), done);
    if (next) {
      setS(next);
      throttleStore.set(next); // the HUD, the status bar and the accent colours follow at once
    }
  };

  const list = apps
    .split("\n")
    .map((a) => a.trim())
    .filter(Boolean);
  const edited = !!s && list.join("\n") !== s.apps.join("\n");

  return (
    <Panel title={t("smart.title")} hint={t("smart.hint")}>
      <label className="flex cursor-pointer items-center gap-2 text-[13px]">
        <input type="checkbox" data-testid="smart-enabled" checked={s?.enabled ?? false} disabled={!s} onChange={(e) => void save({ enabled: e.target.checked }, e.target.checked ? t("smart.on") : t("smart.off"))} />
        {t("smart.enable")}
      </label>

      {s && (
        <p className="mt-2 text-xs" data-testid="smart-status">
          {!s.enabled ? (
            <span className="text-dim">{t("smart.stOff")}</span>
          ) : s.activeApp ? (
            <span className="text-warn">{t("smart.stLimiting", { app: s.activeApp, speed: formatSpeed(s.limitBps ?? 0) })}</span>
          ) : s.scanOk ? (
            <span className="text-ok">{t("smart.stFree")}</span>
          ) : (
            <span className="text-dim">{t("smart.stUnknown")}</span>
          )}
        </p>
      )}

      <div className="mt-3 grid gap-3 sm:grid-cols-[1fr_auto]">
        <label className="space-y-1">
          <span className="label">{t("smart.apps")}</span>
          <textarea data-testid="smart-apps" className="input h-32 py-2 font-mono text-xs" value={apps} onChange={(e) => setApps(e.target.value)}
            onBlur={() => edited && void save({ apps: list }, t("smart.saved"))}
            spellCheck={false}
          />
        </label>
        <label className="space-y-1">
          <span className="label">{t("smart.share")}</span>
          <select data-testid="smart-share" className="input h-9" value={s?.sharePercent ?? 25} disabled={!s} onChange={(e) => void save({ sharePercent: Number(e.target.value) })}>
            {SHARES.map((p) => (
              <option key={p} value={p}>
                {t("smart.shareOpt", { p })}
              </option>
            ))}
          </select>
        </label>
      </div>
      <div className="mt-2 flex items-center gap-2">
        <button className="btn" data-testid="smart-save" disabled={!edited} onClick={() => void save({ apps: list }, t("smart.saved"))}>
          {t("smart.save")}
        </button>
        <p className="text-xs text-dim">{t("smart.discord")}</p>
      </div>
    </Panel>
  );
}
