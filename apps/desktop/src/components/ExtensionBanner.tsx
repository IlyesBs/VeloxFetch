import { useEffect, useState } from "react";
import { actions } from "../actions";
import { api, onExtensionEvent } from "../api";
import { act } from "../hooks";
import { useT } from "../i18n";
import type { BrowserIntegration } from "../types";
import { Icon } from "../ui/icons";
import { Rich } from "../ui/rich";

/** How long "installed and connected" stays up. */
const ARRIVED_FOR_MS = 20_000;

/**
 * The browser extension, on every screen, only when it matters (bridge.rs `ContactLog`):
 * - it reached VeloxFetch for the first time, or the one the first start waited for arrived: "installed and
 *   connected", in green, for a while;
 * - the first start counted on the browser adding it by itself (the installer's pointer) and it has not come after a
 *   few minutes: what the browser may be waiting for, and the store page as the way to do it by hand. "Later" hides
 *   it until the next start.
 */
export function ExtensionBanner() {
  const t = useT();
  const [state, setState] = useState<BrowserIntegration | null>(null);
  const [arrived, setArrived] = useState(false);
  const [overdue, setOverdue] = useState(false);
  const [later, setLater] = useState(false);

  useEffect(() => {
    let off: (() => void) | undefined;
    let cancelled = false;
    // Read again on each event (rare: the first start's wait, a contact at most every few minutes).
    const read = () =>
      api.getBrowserIntegration().then(
        (s) => !cancelled && setState(s),
        () => {},
      );
    void read();
    onExtensionEvent((e) => {
      void read();
      if (e.kind === "contact" && e.announce) setArrived(true);
    }).then((u) => (cancelled ? u() : (off = u)));
    return () => {
      cancelled = true;
      off?.();
    };
  }, []);

  // The wait: overdue once the given time has passed since the first start.
  const since = state?.extensionAwaitedSince ?? null;
  const waitMs = state?.extensionWaitMs ?? 0;
  useEffect(() => {
    setOverdue(false);
    if (since === null) return;
    const timer = setTimeout(() => setOverdue(true), Math.max(0, since + waitMs - Date.now()));
    return () => clearTimeout(timer);
  }, [since, waitMs]);

  useEffect(() => {
    if (!arrived) return;
    const timer = setTimeout(() => setArrived(false), ARRIVED_FOR_MS);
    return () => clearTimeout(timer);
  }, [arrived]);

  const install = async () => {
    const r = await act(() => api.installBrowserExtension(null));
    if (!r) return;
    if (!r.openedBrowser && r.storeUrl) void actions.copy(r.storeUrl);
    setLater(true);
  };

  if (arrived) {
    return (
      <div className="border-b border-line bg-ok/10" role="status" data-testid="extension-banner" data-state="arrived">
        <div className="mx-auto flex max-w-5xl items-center gap-3 px-5 py-2.5">
          <Icon name="check" size={16} className="shrink-0 text-ok" />
          <p className="min-w-0 flex-1 text-[13px]" data-testid="extension-banner-text">
            <Rich text={t("extb.arrived")} />
          </p>
          <button className="btn btn-ghost h-8" data-testid="extension-banner-ok" onClick={() => setArrived(false)}>
            {t("extb.ok")}
          </button>
        </div>
      </div>
    );
  }
  if (!overdue || later || !state?.chromeStoreId) return null;
  return (
    <div className="border-b border-line bg-accent-soft" role="status" data-testid="extension-banner" data-state="waiting">
      <div className="mx-auto flex max-w-5xl flex-wrap items-center gap-3 px-5 py-2.5">
        <Icon name="shield" size={16} className="shrink-0 text-accent" />
        <p className="min-w-0 flex-1 text-[13px]" data-testid="extension-banner-text">
          <Rich text={t("extb.waiting")} />
        </p>
        <button className="btn btn-primary h-8" data-testid="extension-banner-install" onClick={() => void install()}>
          {t("extb.install")}
        </button>
        <button className="btn btn-ghost h-8" data-testid="extension-banner-later" onClick={() => setLater(true)}>
          {t("extb.later")}
        </button>
      </div>
    </div>
  );
}
