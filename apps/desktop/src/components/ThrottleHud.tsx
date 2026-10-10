import { useId } from "react";
import { formatSpeed } from "../format";
import { useStore } from "../hooks";
import { useT } from "../i18n";
import {
  gaugeShare,
  keepWhileActive,
  THROTTLE_MODES as MODES,
  throttleOn,
  throttleStatus,
  throttleStore,
  useThrottle,
} from "../throttle";
import { Icon } from "../ui/icons";
const SWEEP = 0.75; // a 270° gauge, open at the bottom

/** Tick marks around the gauge, for the instrument look. */
function ticks(cx: number, r1: number, r2: number, long: number): string {
  return Array.from({ length: 27 }, (_, i) => {
    const a = ((135 + (i * 270) / 26) * Math.PI) / 180;
    const r = i % 13 === 0 ? long : r2;
    return `M${(cx + r1 * Math.cos(a)).toFixed(2)} ${(cx + r1 * Math.sin(a)).toFixed(2)}L${(cx + r * Math.cos(a)).toFixed(2)} ${(cx + r * Math.sin(a)).toFixed(2)}`;
  }).join("");
}
const SIDE_TICKS = ticks(80, 74, 70, 67);
const WIDE_TICKS = ticks(60, 53, 50, 47);

/**
 * Smart throttle HUD: a radial gauge whose core is the on/off button, a switch, and three modes. The arc
 * shows the share of the connection downloads may use right now (all of it when nothing slows them). Turning
 * it on morphs the whole interface's accent to a stealthy amber (html[data-throttle], see throttle.ts).
 * `side`: the tall card of the sidebar (reference design). `wide`: a row card for the page, used while
 * the sidebar is folded.
 */
export function ThrottleHud({
  variant = "wide",
  className = "",
}: {
  variant?: "side" | "wide";
  className?: string;
}) {
  const t = useT();
  const s = useThrottle();
  const speed = useStore((st) => st.metrics.speedBps);
  const grad = `hud-${useId().replace(/:/g, "")}`;
  const on = throttleOn(s);
  // The active profile turned it on: the switch is the profile's while it is active (change the profile instead).
  const byProfile = s?.profileAdaptive === true && !s.enabled;
  const limiting = on && s?.limitBps != null;
  const share = s?.sharePercent ?? 25;
  // Live: the cap against the speed downloads usually get (the share they keep right now).
  const shown = gaugeShare(s);
  const pct = Math.round(shown * 100);

  const toggle = () =>
    s &&
    void throttleStore.save(
      { enabled: !on },
      on ? t("hud.offToast") : t("hud.on"),
    );
  // Choosing a mode while it is off turns it on in that mode (what choosing one means).
  const pick = (share: number, name: string) =>
    s &&
    (on
      ? share !== s.sharePercent &&
        void throttleStore.save(
          { sharePercent: share },
          t("hud.modeSet", { name }),
        )
      : void throttleStore.save(
          { enabled: true, sharePercent: share },
          t("hud.modeOn", { name }),
        ));

  const status = throttleStatus(t, s);

  const side = variant === "side";
  const size = side ? 160 : 120;
  const c = size / 2;
  const r = side ? 62 : 44;
  const circ = 2 * Math.PI * r;
  const stroke = side ? 9 : 7;

  const gauge = (
    <div
      className="relative mx-auto shrink-0"
      style={{ width: size, height: size }}
    >
      <svg
        viewBox={`0 0 ${size} ${size}`}
        className="absolute inset-0 h-full w-full"
        aria-hidden="true"
      >
        <defs>
          {/* cyan on the left, through blue, to violet on the right (the reference gauge) */}
          <linearGradient id={grad} x1="0" y1="0" x2="1" y2="0">
            <stop offset="0" className="stop-accent" />
            <stop offset="0.6" className="stop-accent-mid" />
            <stop offset="1" className="stop-violet" />
          </linearGradient>
        </defs>
        <path
          d={side ? SIDE_TICKS : WIDE_TICKS}
          className="gauge-track"
          strokeWidth={1.2}
          strokeLinecap="round"
        />
        <circle
          cx={c}
          cy={c}
          r={r}
          fill="none"
          className="gauge-track"
          strokeWidth={stroke}
          strokeLinecap="round"
          strokeDasharray={`${circ * SWEEP} ${circ}`}
          transform={`rotate(135 ${c} ${c})`}
        />
        <circle
          cx={c}
          cy={c}
          r={r}
          fill="none"
          stroke={`url(#${grad})`}
          className={`gauge-grad ${on || !s ? "" : "opacity-80"}`}
          strokeWidth={stroke}
          strokeLinecap="round"
          style={{
            strokeDasharray: `${(circ * SWEEP * shown).toFixed(2)} ${circ.toFixed(2)}`,
          }}
          transform={`rotate(135 ${c} ${c})`}
        />
      </svg>
      <button
        className={`hud-core absolute grid place-items-center rounded-full text-accent ${side ? "inset-[30px]" : "inset-[26px] border border-line bg-raised"} ${limiting ? "hud-live" : ""}`}
        aria-pressed={on}
        aria-label={on ? t("hud.toggleOff") : t("hud.toggleOn")}
        title={byProfile ? t("prof.smartByProfile") : t("hud.gauge", { p: pct })}
        disabled={!s || byProfile}
        data-testid="hud-toggle"
        onClick={toggle}
      >
        <span className="flex flex-col items-center leading-none">
          <Icon
            name="bolt"
            size={side ? 26 : 18}
            className="drop-shadow-[0_0_6px_currentColor]"
          />
          {side ? (
            <>
              <span className="mt-2 text-[16px] font-semibold text-fg">
                {limiting ? t("hud.capped", { p: pct }) : t("hud.unlimited")}
              </span>
              <span className="num ltr mt-1.5 text-[12px] text-dim">
                {formatSpeed(speed)}
              </span>
            </>
          ) : (
            <>
              <span className="num mt-1 text-[15px] font-bold text-fg">{`${pct}%`}</span>
              <span className="mt-0.5 text-[9px] font-semibold uppercase tracking-wider text-dim">
                {on ? t("hud.stealth") : t("hud.boost")}
              </span>
            </>
          )}
        </span>
      </button>
    </div>
  );

  const modes = (
    <div className="flex gap-1.5" role="radiogroup" aria-label={t("hud.mode")}>
      {MODES.map((m) => {
        const name = t(m.name);
        return (
          <button
            key={m.share}
            role="radio"
            aria-checked={share === m.share}
            className="mode-btn"
            title={t("hud.modeTip", {
              name,
              k: keepWhileActive(m.share),
              p: m.share,
            })}
            disabled={!s}
            data-testid={`hud-mode-${m.share}`}
            onClick={() => pick(m.share, name)}
          >
            <Icon name={m.icon} size={17} />
            <span className="mode-label max-w-full truncate">{name}</span>
          </button>
        );
      })}
    </div>
  );

  const head = (
    <div className="flex items-center justify-between gap-3">
      <span className="flex min-w-0 items-center gap-1.5 text-[14px] font-semibold">
        <span className="truncate">{t("hud.title")}</span>
        <span
          className="text-dim"
          title={t("hud.info")}
          aria-label={t("hud.info")}
          role="img"
        >
          <Icon name="info" size={14} />
        </span>
      </span>
      <button
        role="switch"
        aria-checked={on}
        aria-label={on ? t("hud.toggleOff") : t("hud.toggleOn")}
        className="switch"
        title={byProfile ? t("prof.smartByProfile") : undefined}
        disabled={!s || byProfile}
        data-testid="hud-switch"
        onClick={toggle}
      />
    </div>
  );

  const statusLine = (
    <p
      className={`text-[12px] leading-snug ${limiting ? "font-medium text-accent" : "text-dim"}`}
      data-testid="hud-status"
    >
      {status}
    </p>
  );

  if (side) {
    return (
      <div
        className={`relative space-y-4 ${className}`}
        data-testid="throttle-hud"
        data-state={limiting ? "limiting" : on ? "on" : "off"}
      >
        {head}
        {gauge}
        {modes}
        {statusLine}
      </div>
    );
  }
  return (
    <div
      className={`panel relative flex flex-wrap items-center gap-5 ${className}`}
      data-testid="throttle-hud"
      data-state={limiting ? "limiting" : on ? "on" : "off"}
    >
      {gauge}
      <div className="min-w-[14rem] flex-1 space-y-3">
        {head}
        {statusLine}
        {modes}
      </div>
    </div>
  );
}
