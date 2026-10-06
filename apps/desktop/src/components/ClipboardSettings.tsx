import { useCallback, useEffect, useState } from "react";
import { api, normalizeError } from "../api";
import { act, store } from "../hooks";
import { useT } from "../i18n";
import { Rich } from "../ui/rich";
import type { ClipboardState } from "../types";
import { Panel } from "../ui/primitives";

/**
 * Clipboard detection: off by default, because reading the clipboard is something the user should
 * decide. When on, VeloxFetch notices copied download links and offers to download them.
 */
export function ClipboardSettings() {
  const t = useT();
  const [state, setState] = useState<ClipboardState | null>(null);

  const refresh = useCallback(async () => {
    try {
      setState(await api.getClipboardWatch());
    } catch (e) {
      store.toast("error", normalizeError(e).message);
    }
  }, []);
  useEffect(() => void refresh(), [refresh]);

  const toggle = async (enabled: boolean) => {
    const next = await act(() => api.setClipboardWatch(enabled), enabled ? t("clip.on") : t("clip.off"));
    if (next) setState(next);
  };

  return (
    <Panel title={t("clip.title")} hint={t("clip.hint")}>
      <label className="flex cursor-pointer items-center gap-2 text-[13px]">
        <input type="checkbox" data-testid="clipboard-toggle" checked={state?.enabled ?? false} disabled={!state} onChange={(e) => void toggle(e.target.checked)} />
        {t("clip.watch")}
      </label>
      <p className="mt-2 text-xs text-dim">
        <Rich text={t("clip.privacy")} />
      </p>
      {state?.enabled && !state.available && (
        <p className="mt-2 text-xs text-warn" role="status" data-testid="clipboard-unavailable">
          <Rich text={t("clip.unavailable")} />
        </p>
      )}
    </Panel>
  );
}
