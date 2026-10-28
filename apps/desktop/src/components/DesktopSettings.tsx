import { useCallback, useEffect, useState } from "react";
import { api, normalizeError } from "../api";
import { act, store } from "../hooks";
import { useT } from "../i18n";
import type { DesktopPrefs, LoginStart, SetDesktopPrefs } from "../types";
import { Panel } from "../ui/primitives";

/** The ways to start at login this desktop offers (no tray: not "tray"), and the one shown as chosen: without a
 * tray, "tray" starts with the window open, as it did before the choice existed. */
export function loginStartChoices(saved: LoginStart | undefined, noTray: boolean): { current: LoginStart; offered: LoginStart[] } {
  const offered: LoginStart[] = noTray ? ["minimized", "window"] : ["tray", "minimized", "window"];
  const wanted = saved ?? "tray";
  return { current: offered.includes(wanted) ? wanted : "window", offered };
}

/**
 * How VeloxFetch lives on the desktop: keep running in the tray when the window is closed (so browser downloads
 * still arrive), start at login (hidden in the tray, minimized or with the window open), and notify when a download ends while the window is not in front.
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
  const { current: loginStart, offered } = loginStartChoices(prefs?.loginStart, noTray);
  const loginHint = {
    tray: t("desk.startAtLoginHint"),
    minimized: t("desk.startAtLoginMinimizedHint"),
    window: noTray ? t("desk.startAtLoginNoTray") : t("desk.startAtLoginWindowHint"),
  }[loginStart];
  const texts: Record<LoginStart, [string, string]> = {
    tray: [t("desk.loginTray"), t("act.loginTray")],
    minimized: [t("desk.loginMinimized"), t("act.loginMinimized")],
    window: [t("desk.loginWindow"), t("act.loginWindow")],
  };
  const loginChoices = offered.map((value) => [value, ...texts[value]] as const);
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
            loginHint,
          )}
        {prefs?.startAtLoginAvailable !== false && prefs?.startAtLogin && (
          <fieldset className="ms-6 pb-1.5" data-testid="login-start-choice">
            <legend className="text-xs text-dim">{t("desk.loginStart")}</legend>
            <div className="mt-1 flex flex-wrap gap-x-4 gap-y-1">
              {loginChoices.map(([value, label, done]) => (
                <label key={value} className="flex cursor-pointer items-center gap-1.5 text-[13px]">
                  <input
                    type="radio"
                    name="login-start"
                    data-testid={`login-start-${value}`}
                    checked={loginStart === value}
                    onChange={() => void change({ loginStart: value }, done)}
                  />
                  {label}
                </label>
              ))}
            </div>
          </fieldset>
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
