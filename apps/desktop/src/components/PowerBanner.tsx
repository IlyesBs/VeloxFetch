import { useEffect, useState } from "react";
import { actions } from "../actions";
import { useStore } from "../hooks";
import { useT, type Key } from "../i18n";
import { Icon } from "../ui/icons";
import { secondsLeft } from "./when";

const SENTENCE: Record<"shutdown" | "sleep" | "hibernate", Key> = { shutdown: "powb.shutdown", sleep: "powb.sleep", hibernate: "powb.hibernate" };

/** Visible on every screen while a power-action countdown runs, with a Cancel button. */
export function PowerBanner() {
  const t = useT();
  const power = useStore((s) => s.queue?.power);
  const endsAt = power?.phase === "countdown" ? power.countdownEndsAtMs : null;
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (endsAt === null) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 250);
    return () => clearInterval(timer);
  }, [endsAt]);

  if (!power || endsAt === null || !power.action) return null;
  const left = secondsLeft(endsAt, now);
  return (
    <div
      role="alert"
      data-testid="power-banner"
      className="flex items-center gap-3 border-b border-line bg-warn-soft px-5 py-2.5 text-[13px] text-warn"
    >
      <Icon name="power" size={16} />
      <p className="flex-1 font-medium">
        <span data-testid="power-countdown" hidden>
          {left}
        </span>
        {t(SENTENCE[power.action], { s: left })}
      </p>
      <button className="btn" data-testid="power-cancel" onClick={() => void actions.setPowerAction(null)}>
        {t("common.cancel")}
      </button>
    </div>
  );
}
