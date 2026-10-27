import { useState } from "react";
import { api } from "../api";
import { formatBytes } from "../format";
import { act, store, useStore } from "../hooks";
import { useT } from "../i18n";
import { Icon } from "../ui/icons";
import { Rich } from "../ui/rich";

/**
 * An update of the required dependencies, on every screen, only when one was found (manager/tool_updates.rs): its
 * size, and the user's choice. Nothing is downloaded before "Update" (on mobile data that matters). "Skip this
 * version" is remembered (a newer one is offered again); "Later" hides it until the next start.
 */
export function ToolsUpdateBanner() {
  const t = useT();
  const update = useStore((s) => s.videoTools?.update ?? null);
  const [later, setLater] = useState(false);
  const [busy, setBusy] = useState(false);

  if (!update || later) return null;

  const answer = async (accept: boolean) => {
    setBusy(true);
    const tools = await act(() => api.answerVideoToolsUpdate(accept));
    if (tools) store.dispatch({ type: "videoTools", tools });
    setBusy(false);
  };

  return (
    <div className="border-b border-line bg-accent-soft" role="status" data-testid="tools-update-banner">
      <div className="mx-auto flex max-w-5xl flex-wrap items-center gap-3 px-5 py-2.5">
        <Icon name="shield" size={16} className="shrink-0 text-accent" />
        <p className="min-w-0 flex-1 text-[13px]" data-testid="tools-update-text">
          <Rich text={update.size > 0 ? t("toolsu.text", { size: formatBytes(update.size) }) : t("toolsu.textNoSize")} />
        </p>
        <button className="btn btn-primary h-8" data-testid="tools-update-accept" disabled={busy} onClick={() => void answer(true)}>
          {t("toolsu.accept")}
        </button>
        <button className="btn btn-ghost h-8" data-testid="tools-update-decline" disabled={busy} onClick={() => void answer(false)}>
          {t("toolsu.decline")}
        </button>
        <button className="btn btn-ghost h-8" data-testid="tools-update-later" disabled={busy} onClick={() => setLater(true)}>
          {t("toolsu.later")}
        </button>
      </div>
    </div>
  );
}
