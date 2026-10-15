import { formatSpeed } from "../format";
import { useStore } from "../hooks";
import { useT, type Key } from "../i18n";
import { networkLabel, networksInUse, shareOf } from "../networks";
import type { NetworkSpeed } from "../types";
import { Icon } from "../ui/icons";

const STATE: Record<NonNullable<NetworkSpeed["state"]>, { label: Key; dot: string }> = {
  ready: { label: "multi.ready", dot: "bg-dim" },
  working: { label: "multi.working", dot: "bg-ok" },
  resting: { label: "multi.resting", dot: "bg-warn" },
};

/**
 * Several networks at once, live (issue #44): shown on the dashboard while it is on. Each network's speed now, its part
 * of the total (a bar), its connections, and the total: the sum of every network's speed.
 */
export function MultiNetworkCard({ onSettings }: { onSettings: () => void }) {
  const t = useT();
  const s = useStore((st) => st.metrics.networks);
  if (!s) return null;
  const used = networksInUse(s);
  return (
    <section className="panel" data-testid="panel-networks" aria-labelledby="pnn-title">
      <div className="flex items-center justify-between gap-2">
        <h2 id="pnn-title" className="flex min-w-0 items-center gap-2 text-[15px] font-semibold">
          <Icon name="network" size={16} className="shrink-0 text-accent" />
          <span className="truncate">{t("netcard.title")}</span>
        </h2>
        <button className="tool-btn h-8 w-8" aria-label={t("netcard.settings")} title={t("netcard.settings")} onClick={onSettings} data-testid="netcard-settings">
          <Icon name="gear" size={16} />
        </button>
      </div>
      <div className="mt-1 flex items-baseline justify-between gap-2 text-[13px]">
        <span className="text-dim">{t("netcard.total")}</span>
        <span className="num ltr text-[18px] font-semibold text-accent" data-testid="netcard-total">
          {formatSpeed(s.totalBps)}
        </span>
      </div>
      {s.blocked && (
        <p className="mt-2 text-xs text-warn" role="status" data-testid="netcard-blocked">
          {t(s.blocked === "vpn" ? "multi.vpn" : "multi.proxy")}
        </p>
      )}
      {!s.blocked && used < 2 && (
        <p className="mt-2 text-xs text-warn" role="status" data-testid="netcard-one">
          {t("multi.one")}
        </p>
      )}
      <ul className="mt-2 space-y-2.5" data-testid="netcard-list">
        {s.networks.map((n) => {
          const st = n.state ? STATE[n.state] : null;
          return (
            <li key={n.name} className="min-w-0" data-testid="netcard-item" data-name={n.name}>
              <div className="flex min-w-0 items-center gap-2 text-[13px]">
                <span className={`size-1.5 shrink-0 rounded-full ${st ? st.dot : "bg-dim"}`} title={st ? t(st.label) : undefined} aria-hidden />
                <span className="min-w-0 flex-1 truncate font-medium" title={n.name} data-testid="netcard-name">
                  {networkLabel(n, s.networks, t)}
                  {n.main && <span className="ms-1.5 text-[11px] font-normal text-dim">{t(n.chosen ? "multi.main" : "netcard.usualOnly")}</span>}
                </span>
                <span className="num ltr shrink-0 font-semibold" data-testid="netcard-speed">
                  {formatSpeed(n.speedBps)}
                </span>
              </div>
              <div className="mt-1 flex items-center gap-2">
                <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-raised" role="presentation">
                  <div className="h-full rounded-full bg-accent transition-[width] duration-500" style={{ width: `${(shareOf(n.speedBps, s.totalBps) * 100).toFixed(1)}%` }} />
                </div>
                <span className="num shrink-0 text-[11px] text-dim">{t("card.conn", { n: n.connections })}</span>
              </div>
            </li>
          );
        })}
        {s.otherBps > 0 && (
          <li className="flex min-w-0 items-center gap-2 text-[13px]" data-testid="netcard-other">
            <span className="size-1.5 shrink-0 rounded-full bg-dim" aria-hidden />
            <span className="min-w-0 flex-1 truncate text-dim">{t("netcard.other")}</span>
            <span className="num ltr shrink-0 font-semibold">{formatSpeed(s.otherBps)}</span>
          </li>
        )}
      </ul>
    </section>
  );
}

/** The indicator: several networks at once is on, over how many networks (top speed card and status bar). */
export function MultiNetworkBadge({ compact = false }: { compact?: boolean }) {
  const t = useT();
  const s = useStore((st) => st.metrics.networks);
  if (!s) return null;
  const used = networksInUse(s);
  const tip = s.blocked
    ? t(s.blocked === "vpn" ? "multi.vpn" : "multi.proxy")
    : [t("netcard.badgeTip", { n: used }), ...s.networks.map((n) => `${networkLabel(n, s.networks, t)}: ${formatSpeed(n.speedBps)}`)].join("\n");
  return (
    <span
      className={`inline-flex shrink-0 items-center gap-1 rounded-full font-medium ${compact ? "text-accent" : "bg-accent-soft px-2 py-0.5 text-[11.5px] text-accent"} ${s.blocked || used < 2 ? "!text-warn" : ""}`}
      data-testid={compact ? "status-multinet" : "multinet-indicator"}
      data-count={used}
      title={tip}
    >
      <Icon name="network" size={compact ? 13 : 12} className="shrink-0" />
      <span className="num">{s.blocked ? t("netcard.notSpread") : used === 1 ? t("netcard.badgeOne") : t("netcard.badge", { n: used })}</span>
    </span>
  );
}
