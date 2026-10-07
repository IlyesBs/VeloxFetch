import { useCallback, useEffect, useState } from "react";
import { api, normalizeError } from "../api";
import { act, store } from "../hooks";
import { useT } from "../i18n";
import type { DesktopPrefs, SetDesktopPrefs } from "../types";
import { Panel } from "../ui/primitives";

/**
 * How VeloxFetch lives on the desktop: keep running in the tray when the window is closed (so browser downloads
 * still arrive), start in the tray at login, and notify when a download ends while the window is not in front.
 */
export function DesktopSettings() {
  const t = useT();
  const [prefs, setPrefs] = useState<DesktopPrefs | null>(null);

  const refresh = useCallback(async () => {
    try {
      setPrefs(await api.getDesktopPrefs());
    } catch (e) {
      store.toast("error", normalizeError(e).message);
    }
  }, []);
  useEffect(() => void refresh(), [refresh]);

  const change = async (patch: SetDesktopPrefs, done: string) => {
    const next = await act(() => api.setDesktopPrefs(patch), done);
    if (next) setPrefs(next);
  };

  const box = (testid: string, checked: boolean, disabled: boolean, label: string, onChange: (on: boolean) => void, hint: string) => (
    <div className="py-1.5">
      <label className="flex cursor-pointer items-center gap-2 text-[13px]">
        <input type="checkbox" data-testid={testid} checked={checked} disabled={!prefs || disabled} onChange={(e) => onChange(e.target.checked)} />
        {label}
      </label>
      <p className="ms-6 mt-0.5 text-xs text-dim">{hint}</p>
    </div>
  );

  const noTray = prefs !== null && !prefs.trayAvailable;
  return (
    <Panel title={t("desk.title")} hint={t("desk.hint")}>
      <div data-testid="desktop-settings">
        {box(
          "close-to-tray-toggle",
          (prefs?.closeToTray ?? true) && !noTray,
          noTray,
          t("desk.closeToTray"),
          (on) => void change({ closeToTray: on }, on ? t("act.closeToTrayOn") : t("act.closeToTrayOff")),
          noTray ? t("desk.noTray") : t("desk.closeToTrayHint"),
        )}
        {prefs?.startAtLoginAvailable !== false &&
          box(
            "start-at-login-toggle",
            prefs?.startAtLogin ?? false,
            false,
            t("desk.startAtLogin"),
            (on) => void change({ startAtLogin: on }, on ? t("act.startAtLoginOn") : t("act.startAtLoginOff")),
            noTray ? t("desk.startAtLoginNoTray") : t("desk.startAtLoginHint"),
          )}
        {box(
          "notify-finished-toggle",
          prefs?.notifyFinished ?? true,
          false,
          t("desk.notify"),
          (on) => void change({ notifyFinished: on }, on ? t("act.notifyOn") : t("act.notifyOff")),
          t("desk.notifyHint"),
        )}
      </div>
    </Panel>
  );
}
