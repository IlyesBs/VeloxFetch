import { useState } from "react";
import { actions } from "../actions";
import { useStore } from "../hooks";
import { useT, type Key } from "../i18n";
import { Rich } from "../ui/rich";
import type { PowerAction } from "../types";
import { Panel } from "../ui/primitives";

const LABEL: Record<PowerAction, Key> = {
  shutdown: "power.shutdown",
  sleep: "power.sleep",
  hibernate: "power.hibernate",
};

/**
 * "When all downloads finish": a one-shot power action. Nothing happens unless the user picks an
 * action here and presses Arm; it runs once, after a countdown that can be cancelled from anywhere
 * in the app, and is forgotten if VeloxFetch restarts.
 */
export function PowerSettings() {
  const t = useT();
  const power = useStore((s) => s.queue?.power);
  const [choice, setChoice] = useState<PowerAction>("shutdown");

  if (!power) {
    return (
      <Panel title={t("power.title")} hint={t("power.hint")}>
        <p className="text-xs text-dim">{t("common.loading")}</p>
      </Panel>
    );
  }
  if (power.supported.length === 0) {
    return (
      <Panel title={t("power.title")} hint={t("power.hint")}>
        <p className="text-xs text-dim">{t("power.unsupported")}</p>
      </Panel>
    );
  }

  const armed = power.phase !== "off" && power.action !== null;
  const selected = power.supported.includes(choice) ? choice : power.supported[0]!;

  return (
    <Panel title={t("power.title")} hint={t("power.hint")}>
      {armed ? (
        <div className="flex flex-wrap items-center gap-3">
          <p className="min-w-0 flex-1 text-[13px]" data-testid="power-status" role="status">
            <Rich text={t("power.armed", { action: t(LABEL[power.action!]).toLowerCase() })} />
          </p>
          <button className="btn" data-testid="power-disarm" onClick={() => void actions.setPowerAction(null)}>
            {t("common.cancel")}
          </button>
        </div>
      ) : (
        <>
          <div role="radiogroup" aria-label={t("power.action")} className="inline-flex rounded-xl border border-line bg-raised p-1">
            {power.supported.map((a) => (
              <button
                key={a}
                role="radio"
                aria-checked={selected === a}
                data-testid={`power-option-${a}`}
                onClick={() => setChoice(a)}
                className={`h-8 rounded-lg px-3 text-[13px] font-medium transition-colors duration-150 ${
                  selected === a ? "bg-surface text-fg shadow-sm" : "text-dim hover:text-fg"
                }`}
              >
                {t(LABEL[a])}
              </button>
            ))}
          </div>
          <div className="mt-3 flex flex-wrap items-center gap-3">
            <button className="btn btn-primary" data-testid="power-arm" onClick={() => void actions.setPowerAction(selected)}>
              {t("power.arm", { action: t(LABEL[selected]).toLowerCase() })}
            </button>
            <span className="text-xs text-dim" data-testid="power-status">
              {t("power.notArmed")}
            </span>
          </div>
        </>
      )}
      <p className="mt-3 text-xs text-dim">
        {t("power.how", { secs: power.graceSecs })}
      </p>
      {power.lastError && (
        <p className="mt-2 text-xs text-bad" role="alert" data-testid="power-error">
          {t("power.failed", { why: power.lastError })}
        </p>
      )}
    </Panel>
  );
}
