import { useEffect, useState } from "react";
import { api, normalizeError } from "../api";
import { act, store } from "../hooks";
import { useT } from "../i18n";
import { Icon } from "../ui/icons";
import { Panel } from "../ui/primitives";

/**
 * The publisher's checksum (feature #6): on by default. VeloxFetch looks for the checksum list a site publishes next
 * to the file (SHA256SUMS…) and checks the file against it before it is finished.
 */
export function IntegritySettings() {
  const t = useT();
  const [on, setOn] = useState<boolean | null>(null);
  useEffect(() => {
    api.getPublisherCheck().then(setOn, (e) => store.toast("error", normalizeError(e).message));
  }, []);
  const toggle = async (enabled: boolean) => {
    const next = await act(() => api.setPublisherCheck(enabled), enabled ? t("verify.setOn") : t("verify.setOff"));
    if (next !== undefined) setOn(next);
  };
  return (
    <Panel title={t("verify.setTitle")} hint={t("verify.setHint")}>
      <div className="flex items-start gap-3" data-testid="integrity-settings">
        <Icon name="shield" size={22} className={`mt-0.5 shrink-0 ${on ? "text-ok" : "text-dim"}`} />
        <div className="min-w-0 flex-1 space-y-2">
          <label className="flex cursor-pointer items-center gap-2 text-[13px] font-medium">
            <input type="checkbox" data-testid="integrity-toggle" checked={on ?? false} disabled={on === null} onChange={(e) => void toggle(e.target.checked)} />
            {t("verify.setLabel")}
          </label>
          <p className="text-xs text-dim">{t("verify.setWhat")}</p>
          <p className="text-xs text-dim">{t("verify.setLimit")}</p>
        </div>
      </div>
    </Panel>
  );
}
