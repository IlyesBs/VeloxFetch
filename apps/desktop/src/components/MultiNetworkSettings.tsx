import { useCallback, useEffect, useState } from "react";
import { api, normalizeError } from "../api";
import { act, store } from "../hooks";
import { useT, type Key } from "../i18n";
import { formatBytes } from "../format";
import { networkLabel } from "../networks";
import { throttleStore } from "../throttle";
import type { MultiNetwork, NetworkLane } from "../types";
import { Panel } from "../ui/primitives";

/** How often the list is read again while Settings is open (networks come and go; what each one is doing). */
const REFRESH_MS = 3000;

const STATE: Record<NonNullable<NetworkLane["state"]>, { label: Key; dot: string }> = {
  ready: { label: "multi.ready", dot: "bg-dim" },
  working: { label: "multi.working", dot: "bg-ok" },
  resting: { label: "multi.resting", dot: "bg-warn" },
};

/**
 * Several networks at once: off by default. When on, a download's connections are spread over the chosen networks
 * (Wi-Fi, a cable, a shared phone), so their speeds add up. Each network can be left out.
 */
export function MultiNetworkSettings() {
  const t = useT();
  const [state, setState] = useState<MultiNetwork | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async (loud: boolean) => {
    try {
      setState(await api.getMultiNetwork());
    } catch (e) {
      if (loud) store.toast("error", normalizeError(e).message);
    }
  }, []);
  useEffect(() => {
    void refresh(true);
    const timer = window.setInterval(() => void refresh(false), REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [refresh]);

  const save = async (enabled: boolean, skip: string[], done: string) => {
    setBusy(true);
    const next = await act(() => api.setMultiNetwork({ enabled, skip }), done);
    setBusy(false);
    if (next) setState(next);
    // The dashboard's indicator and card appear / go at once, even with nothing downloading.
    api.getMetrics().then(store.setMetrics, () => {});
    // Smart throttle is held off while it is on: the sidebar shows it at once.
    void throttleStore.refresh();
  };

  const skipped = (s: MultiNetwork) => s.networks.filter((n) => !n.chosen).map((n) => n.name);
  const choose = (s: MultiNetwork, name: string, on: boolean) => {
    const rest = skipped(s).filter((n) => n !== name);
    void save(s.enabled, on ? rest : [...rest, name], t("multi.saved"));
  };
  const chosen = state?.networks.filter((n) => n.chosen).length ?? 0;

  return (
    <Panel id="multi-network" title={t("multi.title")} hint={t("multi.hint")}>
      <label className="flex cursor-pointer items-center gap-2 text-[13px]">
        <input
          type="checkbox"
          data-testid="multinet-toggle"
          checked={state?.enabled ?? false}
          disabled={!state || busy}
          onChange={(e) => state && void save(e.target.checked, skipped(state), e.target.checked ? t("multi.on") : t("multi.off"))}
        />
        {t("multi.use")}
      </label>
      <p className="mt-1.5 text-xs text-dim">{t("multi.rules")}</p>

      {state?.enabled && state.blocked && (
        <p className="mt-2 text-xs text-warn" role="status" data-testid="multinet-blocked">
          {t(state.blocked === "vpn" ? "multi.vpn" : "multi.proxy")}
        </p>
      )}

      {state && state.networks.length === 0 && (
        <p className="mt-3 text-xs text-dim" data-testid="multinet-none">
          {t("multi.none")}
        </p>
      )}
      {state && state.networks.length > 0 && (
        <>
          <ul className="mt-3 divide-y divide-line rounded-xl border border-line bg-raised" data-testid="multinet-list">
            {state.networks.map((n) => {
              const st = n.state ? STATE[n.state] : null;
              return (
                <li key={n.name} className="flex min-w-0 items-center gap-3 px-3 py-2 text-[13px]" data-testid="multinet-item" data-name={n.name}>
                  <label className="flex min-w-0 flex-1 cursor-pointer items-center gap-2">
                    <input type="checkbox" checked={n.chosen} disabled={busy} onChange={(e) => choose(state, n.name, e.target.checked)} />
                    <span className="min-w-0 truncate font-medium" title={n.name} data-testid="multinet-name">
                      {networkLabel(n, state.networks, t)}
                    </span>
                    {n.main && <span className="shrink-0 rounded-md bg-surface px-1.5 py-0.5 text-[11px] text-dim">{t("multi.main")}</span>}
                  </label>
                  {st && (
                    <span className="flex shrink-0 items-center gap-1.5 text-xs text-dim" data-testid="multinet-state" data-state={n.state}>
                      <span className={`size-1.5 rounded-full ${st.dot}`} aria-hidden />
                      {t(st.label)}
                    </span>
                  )}
                  {n.bytes > 0 && <span className="num shrink-0 text-xs text-dim">{t("multi.received", { size: formatBytes(n.bytes) })}</span>}
                </li>
              );
            })}
          </ul>
          <p className="mt-1.5 text-xs text-dim">{t("multi.leaveOut")}</p>
          {state.enabled && !state.blocked && chosen < 2 && (
            <p className="mt-2 text-xs text-warn" role="status" data-testid="multinet-one">
              {t(state.networks.length < 2 ? "multi.one" : "multi.pickTwo")}
            </p>
          )}
        </>
      )}
    </Panel>
  );
}
