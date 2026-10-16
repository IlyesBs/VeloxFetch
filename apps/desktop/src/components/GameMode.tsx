import { useEffect, useState } from "react";
import { useT, type Key } from "../i18n";
import {
  formatLeft,
  GAME_DURATIONS,
  gameLine,
  gameOn,
  gameSecondsLeft,
  throttleStore,
  useThrottle,
} from "../throttle";
import type { SmartThrottle } from "../types";
import { Icon } from "../ui/icons";

// Game mode (feature #7): one click and downloads give way to a game or a call now, without opening Settings and
// without waiting for a listed program. On from the status bar, the tray icon or Ctrl+G; the banner on every screen
// says it is on, what downloads do, when it ends, with "+1 hour" and "Turn off". What downloads do is the gaming
// policy (Settings › Speed › Smart throttle).

type T = (k: Key, v?: Record<string, string | number>) => string;

/** The current time, every second while Game mode runs for a time (for "ends in"). */
function useNow(s: SmartThrottle | null): number {
  const ticking = gameOn(s) && s?.gameMode.untilMs != null;
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!ticking) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [ticking]);
  return now;
}

/** On for the usual time, or off: what one click does (the status bar, Ctrl+G). */
export function toggleGameMode(t: T): void {
  const s = throttleStore.get();
  if (!s) return;
  if (gameOn(s)) void throttleStore.game({ on: false }, t("gm.offToast"));
  else void throttleStore.game({ on: true }, t("gm.onToast"));
}

/** "until you turn it off" / "1 h 42 min left". */
function leftText(t: T, s: SmartThrottle, now: number): string {
  const left = gameSecondsLeft(s.gameMode, now);
  return left === null ? t("gm.untilOff") : t("gm.left", { time: formatLeft(t, left) });
}

/** The status bar's Game mode button: one click on, one click off; while on, how long is left. */
export function GameModeButton() {
  const t = useT();
  const s = useThrottle();
  const now = useNow(s);
  if (!s) return null;
  const on = gameOn(s);
  return (
    <button
      type="button"
      className={`game-btn flex min-w-0 shrink-0 items-center gap-1.5 ${on ? "game-btn-on" : ""}`}
      data-testid="status-game"
      data-state={on ? "on" : "off"}
      aria-pressed={on}
      aria-label={on ? `${t("gm.on")} · ${leftText(t, s, now)}` : t("gm.name")}
      title={on ? `${t("gm.name")} · ${gameLine(t, s)} · ${leftText(t, s, now)} — ${t("gm.clickOff")}` : t("gm.tip")}
      onClick={() => toggleGameMode(t)}
    >
      <Icon name="gamepad" size={15} className="shrink-0" />
      {/* Narrow windows: the lit pill and the time left (the banner and the tooltip have the words). */}
      <span className={on ? "hidden xl:inline" : "hidden lg:inline"}>{on ? t("gm.on") : t("gm.name")}</span>
      {on && (
        <span className="num" data-testid="status-game-left">
          <span className="hidden xl:inline">· </span>
          {s.gameMode.untilMs === null ? "∞" : formatLeft(t, gameSecondsLeft(s.gameMode, now) ?? 0)}
        </span>
      )}
    </button>
  );
}

/** Visible on every screen while Game mode is on: what downloads do, when it ends, +1 hour and Turn off. */
export function GameModeBanner() {
  const t = useT();
  const s = useThrottle();
  const now = useNow(s);
  if (!s || !gameOn(s)) return null;
  const timed = s.gameMode.untilMs !== null;
  return (
    <div
      role="status"
      data-testid="game-banner"
      className="game-banner flex flex-wrap items-center gap-x-3 gap-y-1.5 border-b border-line px-5 py-2 text-[13px]"
    >
      <span className="game-banner-icon grid h-7 w-7 shrink-0 place-items-center rounded-full">
        <Icon name="gamepad" size={16} />
      </span>
      <p className="min-w-0 flex-1">
        <span className="font-semibold">{t("gm.on")}</span>
        <span className="text-fg/85"> · {gameLine(t, s)}</span>
        <span className="num ms-2 text-dim" data-testid="game-banner-left">
          {leftText(t, s, now)}
        </span>
      </p>
      <span className="flex shrink-0 items-center gap-2">
        {timed && (
          <button className="btn" data-testid="game-more" onClick={() => void throttleStore.game({ addMinutes: 60 }, t("gm.moreToast"))}>
            {t("gm.plusHour")}
          </button>
        )}
        <button className="btn btn-primary" data-testid="game-off" onClick={() => void throttleStore.game({ on: false }, t("gm.offToast"))}>
          {t("gm.turnOff")}
        </button>
      </span>
    </div>
  );
}

/** Settings › Speed: Game mode on or off now, and how long one click turns it on for. */
export function GameModeSettings() {
  const t = useT();
  const s = useThrottle();
  const now = useNow(s);
  const on = gameOn(s);
  return (
    <section className={`game-card mb-4 rounded-lg border p-3 ${on ? "game-card-on" : "border-line"}`} data-testid="game-settings">
      <div className="flex flex-wrap items-center gap-3">
        <span className="game-banner-icon grid h-9 w-9 shrink-0 place-items-center rounded-full">
          <Icon name="gamepad" size={19} />
        </span>
        <div className="min-w-[12rem] flex-1">
          <p className="text-[14px] font-semibold">{t("gm.name")}</p>
          <p className="text-xs text-dim">{t("gm.hint")}</p>
        </div>
        <button
          className={`btn shrink-0 ${on ? "" : "btn-primary"}`}
          data-testid="game-settings-toggle"
          aria-pressed={on}
          disabled={!s}
          onClick={() => toggleGameMode(t)}
        >
          {on ? t("gm.turnOff") : t("gm.turnOn")}
        </button>
      </div>
      {s && (
        <p className={`mt-2 text-xs ${on ? "font-medium text-accent" : "text-dim"}`} role="status" data-testid="game-settings-status">
          {on ? `${t("gm.on")} · ${gameLine(t, s)} · ${leftText(t, s, now)}` : t("gm.offStatus", { line: gameLine(t, s) })}
        </p>
      )}
      <label className="mt-3 flex flex-wrap items-center gap-2 text-[13px]">
        <span className="label">{t("gm.duration")}</span>
        <select
          className="input h-8 w-auto"
          data-testid="game-duration"
          value={s?.gameMode.minutes ?? 120}
          disabled={!s}
          onChange={(e) => void throttleStore.game({ defaultMinutes: Number(e.target.value) }, t("smart.updated"))}
        >
          {GAME_DURATIONS.map((m) => (
            <option key={m} value={m}>
              {t(`gm.d${m}`)}
            </option>
          ))}
          {s && !(GAME_DURATIONS as readonly number[]).includes(s.gameMode.minutes) && (
            <option value={s.gameMode.minutes}>{formatLeft(t, s.gameMode.minutes * 60)}</option>
          )}
        </select>
        <span className="text-xs text-dim">{t("gm.shortcut")}</span>
      </label>
    </section>
  );
}
