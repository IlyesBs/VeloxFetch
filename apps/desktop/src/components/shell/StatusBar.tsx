import { formatBytes, formatSpeed } from "../../format";
import { useStore } from "../../hooks";
import { useT } from "../../i18n";
import { gameOn, throttleOn, throttleStatus, useThrottle } from "../../throttle";
import { Icon } from "../../ui/icons";
import { GameModeButton } from "../GameMode";
import { MultiNetworkBadge } from "../MultiNetworkCard";
import { ProfileSwitch } from "../ProfilesSettings";
import { updateLine } from "../UpdateSettings";

/** The footer across the window: engine state, counts, the total, live speed, Game mode, Smart throttle, free space, updates. */
export function StatusBar({ onUpdates }: { onUpdates?: () => void }) {
  const t = useT();
  const ready = useStore((s) => s.ready);
  const update = useStore((s) => s.update);
  const newer = update?.supported && (update.phase === "available" || update.phase === "ready" || update.phase === "elsewhere" || update.phase === "downloading");
  const m = useStore((s) => s.metrics);
  const throttle = useThrottle();

  return (
    <footer className="statusbar" data-testid="statusbar">
      <span
        className="flex items-center gap-2"
        title={ready ? t("foot.online") : t("foot.starting")}
      >
        {ready ? (
          <span className="live-dot h-2 w-2" />
        ) : (
          <span className="inline-block h-2 w-2 rounded-full bg-dim" />
        )}
        <span className="num text-fg/90">
          {ready ? t("foot.active", { n: m.active }) : t("foot.starting")}
        </span>
      </span>
      <span className="sep" />
      <span className="num">{t("foot.inQueue", { n: m.queued })}</span>
      {/* Narrow windows keep what matters most (counts, speed, Game mode); long languages never push anything out. */}
      <span className="sep hidden md:block" />
      <span className="num hidden md:inline">
        {t("foot.downloaded", { size: formatBytes(m.totalDownloaded) })}
      </span>
      <span className="sep" />
      <span
        className="num ltr flex items-center gap-1 text-fg/80"
        data-testid="status-speed"
      >
        <Icon name="arrowDown" size={13} className="text-accent" />
        {formatSpeed(m.speedBps)}
      </span>
      {m.networks && (
        <>
          <span className="sep hidden sm:block" />
          <MultiNetworkBadge compact />
        </>
      )}
      <span className="sep hidden md:block" />
      <span className="hidden md:flex">
        <ProfileSwitch />
      </span>
      <span className="sep" />
      <GameModeButton />
      {throttle && throttleOn(throttle) && !gameOn(throttle) && (
        <span
          className="flex min-w-0 items-center gap-1 truncate font-medium text-accent"
          data-testid="status-throttle"
          title={throttleStatus(t, throttle)}
        >
          <Icon name="bolt" size={13} className="shrink-0" />
          <span className="truncate">
            {throttle.activeApp
              ? t("foot.limiting", { app: throttle.activeApp })
              : throttle.limitBps !== null
                ? t("foot.slowed", { speed: formatSpeed(throttle.limitBps) })
                : t("foot.stealth")}
          </span>
        </span>
      )}
      <span className="flex-1" />
      {m.diskFree !== null && (
        <span className="num hidden items-center gap-1 lg:flex">
          <Icon name="hdd" size={13} />
          {t("foot.free", { size: formatBytes(m.diskFree) })}
        </span>
      )}
      {update?.supported && (newer || update.phase === "upToDate") && (
        <button
          type="button"
          className={`flex items-center gap-1 ${newer ? "font-medium text-accent" : "text-fg/70"}`}
          data-testid="status-update"
          title={updateLine(t, update)}
          onClick={onUpdates}
        >
          <Icon name={newer ? "download" : "checkCircle"} size={13} />
          <span className={newer ? "" : "hidden lg:inline"}>
            {newer ? t(update.phase === "ready" ? "foot.updateReady" : "foot.updateAvailable", { v: update.version ?? "" }) : t("foot.upToDate")}
          </span>
        </button>
      )}
      <span
        className="flex items-center gap-1.5 text-fg/85"
        title={t("foot.private")}
      >
        <Icon name="shield" size={17} className="text-ok" />
        <span className="hidden xl:inline">{t("foot.private")}</span>
      </span>
    </footer>
  );
}
