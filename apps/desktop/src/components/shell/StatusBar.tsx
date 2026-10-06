import { formatBytes, formatSpeed } from "../../format";
import { useStore } from "../../hooks";
import { useT } from "../../i18n";
import { useThrottle } from "../../throttle";
import { Icon } from "../../ui/icons";

/** The footer across the window: engine state, counts, the total, live speed, Smart throttle and free space. */
export function StatusBar() {
  const t = useT();
  const ready = useStore((s) => s.ready);
  const m = useStore((s) => s.metrics);
  const throttle = useThrottle();

  return (
    <footer className="statusbar" data-testid="statusbar">
      <span className="flex items-center gap-2" title={ready ? t("foot.online") : t("foot.starting")}>
        {ready ? <span className="live-dot h-2 w-2" /> : <span className="inline-block h-2 w-2 rounded-full bg-dim" />}
        <span className="num text-fg/90">{ready ? t("foot.active", { n: m.active }) : t("foot.starting")}</span>
      </span>
      <span className="sep" />
      <span className="num">{t("foot.inQueue", { n: m.queued })}</span>
      <span className="sep hidden sm:block" />
      <span className="num hidden sm:inline">{t("foot.downloaded", { size: formatBytes(m.totalDownloaded) })}</span>
      <span className="sep hidden md:block" />
      <span className="num ltr flex items-center gap-1 text-fg/80" data-testid="status-speed">
        <Icon name="arrowDown" size={13} className="text-accent" />
        {formatSpeed(m.speedBps)}
      </span>
      {throttle?.enabled && (
        <span className="flex min-w-0 items-center gap-1 truncate font-medium text-accent" data-testid="status-throttle">
          <Icon name="bolt" size={13} className="shrink-0" />
          <span className="truncate">{throttle.activeApp ? t("foot.limiting", { app: throttle.activeApp }) : t("foot.stealth")}</span>
        </span>
      )}
      <span className="flex-1" />
      {m.diskFree !== null && (
        <span className="num hidden items-center gap-1 lg:flex">
          <Icon name="hdd" size={13} />
          {t("foot.free", { size: formatBytes(m.diskFree) })}
        </span>
      )}
      <span className="flex items-center gap-1.5 text-fg/85" title={t("foot.private")}>
        <Icon name="shield" size={17} className="text-ok" />
        <span className="hidden md:inline">{t("foot.private")}</span>
      </span>
    </footer>
  );
}
