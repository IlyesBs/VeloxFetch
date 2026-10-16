import { useEffect, useId, useState } from "react";
import { GameModeSettings } from "./GameMode";
import { actions } from "../actions";
import { useStore } from "../hooks";
import { formatSpeed } from "../format";
import { useT } from "../i18n";
import {
  keepWhileActive,
  LEVEL_REQUEST,
  THROTTLE_LEVELS,
  THROTTLE_MODES,
  throttleLevel,
  throttleOn,
  throttleStatus,
  throttleStore,
  useThrottle,
  useThrottleHistory,
  type ThrottlePoint,
} from "../throttle";
import type { GamePolicy, SetSmartThrottle } from "../types";
import { Panel } from "../ui/primitives";

const KB = 1024;
const MB = 1024 * 1024;
/** The reserved minimum: a few clear steps (a value set elsewhere is shown as it is). */
const MINIMUMS = [
  64 * KB,
  128 * KB,
  256 * KB,
  512 * KB,
  1 * MB,
  2 * MB,
  5 * MB,
  10 * MB,
];
/** The low-bandwidth gaming policy's limit. */
const GAME_LIMITS = [64 * KB, 128 * KB, 256 * KB, 512 * KB, 1 * MB, 2 * MB, 5 * MB];
const GAME_POLICIES: { value: GamePolicy; label: "smart.policyAdaptive" | "smart.policyLow" | "smart.policyPause" }[] = [
  { value: "adaptive", label: "smart.policyAdaptive" },
  { value: "low", label: "smart.policyLow" },
  { value: "pause", label: "smart.policyPause" },
];
/** The maximum download speed (the global speed limit). */
const MAXIMUMS = [1 * MB, 2 * MB, 5 * MB, 10 * MB, 20 * MB, 50 * MB, 100 * MB];

/** Download speed over the last minutes (filled), with the Smart throttle limit as a dashed line where it applied. */
function BandwidthGraph({
  points,
  label,
  capLabel,
}: {
  points: ThrottlePoint[];
  label: string;
  capLabel: string;
}) {
  const id = useId().replace(/:/g, "");
  const W = 100;
  const H = 30;
  const n = points.length;
  const max = Math.max(1, ...points.map((p) => Math.max(p.speed, p.cap ?? 0)));
  const x = (i: number) => (n <= 1 ? W : (i / (n - 1)) * W);
  const y = (v: number) => H - 1 - (v / max) * (H - 4);
  const line = points
    .map(
      (p, i) => `${i ? "L" : "M"}${x(i).toFixed(2)} ${y(p.speed).toFixed(2)}`,
    )
    .join(" ");
  // The cap as separate runs: a break where nothing limited downloads.
  let cap = "";
  points.forEach((p, i) => {
    if (p.cap === null) return;
    cap += `${i === 0 || points[i - 1]?.cap == null ? "M" : "L"}${x(i).toFixed(2)} ${y(p.cap).toFixed(2)} `;
  });
  return (
    <figure className="mt-3" data-testid="smart-graph">
      <svg
        viewBox={`0 0 ${W} ${H}`}
        preserveAspectRatio="none"
        role="img"
        aria-label={label}
        className="block h-24 w-full rounded-lg border border-line bg-raised"
      >
        <defs>
          <linearGradient id={`bg${id}`} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0" className="stop-accent" stopOpacity={0.32} />
            <stop offset="1" className="stop-accent" stopOpacity={0} />
          </linearGradient>
        </defs>
        {n > 1 && (
          <>
            <path d={`${line} L${W} ${H} L0 ${H} Z`} fill={`url(#bg${id})`} />
            <path
              d={line}
              fill="none"
              className="spark-line stroke-accent"
              strokeWidth={2}
              vectorEffect="non-scaling-stroke"
              strokeLinejoin="round"
            />
          </>
        )}
        {cap && (
          <path
            d={cap}
            fill="none"
            className="stroke-warn"
            strokeWidth={1.4}
            strokeDasharray="3 3"
            vectorEffect="non-scaling-stroke"
            data-testid="smart-graph-cap"
          />
        )}
      </svg>
      <figcaption className="mt-1 flex items-center justify-between text-[11px] text-dim">
        <span>{label}</span>
        <span className="flex items-center gap-1.5">
          <span
            className="inline-block w-4 border-t border-dashed border-warn"
            aria-hidden="true"
          />
          {capLabel}
        </span>
      </figcaption>
    </figure>
  );
}

/**
 * Settings > Speed > Smart throttle: downloads give way while the user browses, watches or plays (automatic
 * mode), or while a listed program runs, and take the whole connection back when it is free. Everything is
 * measured on this computer only; nothing is recorded or sent.
 */
export function SmartThrottleSettings() {
  const t = useT();
  // The one shared state (polled every 3 s by the app shell): this panel, the dashboard card and the status bar
  // always show the same thing.
  const s = useThrottle();
  const [apps, setApps] = useState("");
  const [appsLoaded, setAppsLoaded] = useState(false);
  const history = useThrottleHistory();
  const globalLimit = useStore((st) => st.metrics.globalLimit);

  useEffect(() => {
    if (!s) void throttleStore.refresh();
    else if (!appsLoaded) {
      setApps(s.apps.join("\n"));
      setAppsLoaded(true);
    }
  }, [s, appsLoaded]);

  const save = (patch: SetSmartThrottle, done?: string) =>
    void throttleStore.save(patch, done);

  const list = apps
    .split("\n")
    .map((a) => a.trim())
    .filter(Boolean);
  const edited = !!s && list.join("\n") !== s.apps.join("\n");
  // The level in force (a Download Profile may set it: Fast, or Balanced's Smart).
  const level = throttleLevel(s);
  const byProfile = s?.profileAdaptive != null;
  // Several networks at once is on: Smart throttle stays off (every network at full speed) until it is turned off.
  const held = !!s?.heldByNetworks;
  const on = throttleOn(s);
  const slowed = on && s?.limitBps != null;
  const share = s?.sharePercent ?? 25;
  const named = THROTTLE_MODES.some((m) => m.share === share);
  const minimums =
    s && !MINIMUMS.includes(s.minBps)
      ? [...MINIMUMS, s.minBps].sort((a, b) => a - b)
      : MINIMUMS;
  const policy = s?.gamePolicy ?? "adaptive";
  const gameLimits =
    s && !GAME_LIMITS.includes(s.gameLimitBps)
      ? [...GAME_LIMITS, s.gameLimitBps].sort((a, b) => a - b)
      : GAME_LIMITS;
  const maximums =
    globalLimit && !MAXIMUMS.includes(globalLimit)
      ? [...MAXIMUMS, globalLimit].sort((a, b) => a - b)
      : MAXIMUMS;

  return (
    <Panel title={t("smart.title")} hint={t("smart.hint")}>
      <GameModeSettings />
      <fieldset>
        <legend className="label mb-1.5">{t("smart.level")}</legend>
        <div className="grid gap-2 sm:grid-cols-3" role="radiogroup" data-testid="smart-levels">
          {THROTTLE_LEVELS.map((l) => (
            <label
              key={l.level}
              className={`flex cursor-pointer gap-2 rounded-md border p-2.5 text-[13px] ${level === l.level ? "border-accent" : "border-line"} ${!s || byProfile || held ? "cursor-not-allowed opacity-60" : ""}`}
            >
              <input
                type="radio"
                name="smart-level"
                className="mt-0.5"
                data-testid={`smart-level-${l.level}`}
                checked={level === l.level}
                disabled={!s || byProfile || held}
                onChange={() => save(LEVEL_REQUEST[l.level], t("hud.modeSet", { name: t(`lvl.${l.level}`) }))}
              />
              <span>
                <span className="font-medium">{t(`lvl.${l.level}`)}</span>
                <span className="mt-0.5 block text-xs text-dim">{t(`lvl.${l.level}Hint`)}</span>
              </span>
            </label>
          ))}
        </div>
      </fieldset>
      {held && (
        <p className="mt-1 text-[11px] text-accent" data-testid="smart-held-note">
          {t("multi.smartHeld")}
        </p>
      )}
      {s?.profileAdaptive != null && !held && (
        <p className="mt-1 text-[11px] text-accent" data-testid="smart-profile-note">
          {t(s.profileAdaptive ? "prof.smartAutoOn" : "prof.smartAutoOff")}
        </p>
      )}

      {s && (
        <p
          className={`mt-2 text-xs ${slowed ? "font-medium text-warn" : on ? "text-ok" : "text-dim"}`}
          role="status"
          data-testid="smart-status"
        >
          {throttleStatus(t, s)}
        </p>
      )}

      <BandwidthGraph
        points={history}
        label={t("smart.graph")}
        capLabel={t("smart.graphCap")}
      />

      {level === "smart" && <p className="mt-3 text-xs text-dim">{t("smart.autoHint")}</p>}

      <div className="mt-4 grid gap-3 sm:grid-cols-3">
        <label className="space-y-1">
          <span className="label">{t("smart.mode")}</span>
          <select
            data-testid="smart-mode"
            className="input h-9"
            value={share}
            disabled={!s || level !== "manual"}
            onChange={(e) =>
              save({ sharePercent: Number(e.target.value) }, t("smart.updated"))
            }
          >
            {THROTTLE_MODES.map((m) => (
              <option
                key={m.share}
                value={m.share}
                title={t("hud.modeTip", {
                  name: t(m.name),
                  k: keepWhileActive(m.share),
                })}
              >
                {t(m.name)}
              </option>
            ))}
            {!named && (
              <option value={share}>
                {t("smart.modeCustom", { p: share })}
              </option>
            )}
          </select>
        </label>
        <label className="space-y-1">
          <span className="label">{t("smart.min")}</span>
          <select
            data-testid="smart-min"
            className="input h-9"
            value={s?.minBps ?? 256 * KB}
            disabled={!s}
            onChange={(e) =>
              save({ minBps: Number(e.target.value) }, t("smart.updated"))
            }
          >
            {minimums.map((v) => (
              <option key={v} value={v}>
                {formatSpeed(v)}
              </option>
            ))}
          </select>
        </label>
        <label className="space-y-1">
          <span className="label">{t("smart.max")}</span>
          <select
            data-testid="smart-max"
            className="input h-9"
            value={globalLimit ?? 0}
            title={t("smart.maxHint")}
            onChange={(e) =>
              void actions.setGlobalLimit(Number(e.target.value) || null)
            }
          >
            <option value={0}>{t("smart.maxNone")}</option>
            {maximums.map((v) => (
              <option key={v} value={v}>
                {formatSpeed(v)}
              </option>
            ))}
          </select>
        </label>
      </div>

      <div className="mt-4 border-t border-line pt-3">
        <div className="mb-3 grid gap-3 sm:grid-cols-3">
          <label className="space-y-1 sm:col-span-2">
            <span className="label">{t("smart.policy")}</span>
            <select
              data-testid="smart-policy"
              className="input h-9"
              value={policy}
              disabled={!s}
              onChange={(e) =>
                save({ gamePolicy: e.target.value as GamePolicy }, t("smart.updated"))
              }
            >
              {GAME_POLICIES.map((p) => (
                <option key={p.value} value={p.value}>
                  {t(p.label)}
                </option>
              ))}
            </select>
          </label>
          {policy === "low" && (
            <label className="space-y-1">
              <span className="label">{t("smart.policyLimit")}</span>
              <select
                data-testid="smart-policy-limit"
                className="input h-9"
                value={s?.gameLimitBps ?? 512 * KB}
                disabled={!s}
                onChange={(e) =>
                  save({ gameLimitBps: Number(e.target.value) }, t("smart.updated"))
                }
              >
                {gameLimits.map((v) => (
                  <option key={v} value={v}>
                    {formatSpeed(v)}
                  </option>
                ))}
              </select>
            </label>
          )}
          <p className="text-xs text-dim sm:col-span-3">{t("smart.policyHint")}</p>
        </div>
        <label className="space-y-1">
          <span className="label">{t("smart.appsTitle")}</span>
          <span className="block text-xs text-dim">{t("smart.apps")}</span>
          <textarea
            data-testid="smart-apps"
            className="input h-28 py-2 font-mono text-xs"
            value={apps}
            onChange={(e) => setApps(e.target.value)}
            onBlur={() => edited && save({ apps: list }, t("smart.saved"))}
            spellCheck={false}
          />
        </label>
        <div className="mt-2 flex items-center gap-2">
          <button
            className="btn"
            data-testid="smart-save"
            disabled={!edited}
            onClick={() => save({ apps: list }, t("smart.saved"))}
          >
            {t("smart.save")}
          </button>
          <p className="text-xs text-dim">{t("smart.discord")}</p>
        </div>
      </div>
    </Panel>
  );
}
