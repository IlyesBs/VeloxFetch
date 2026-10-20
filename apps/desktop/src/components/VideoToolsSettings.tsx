import { useCallback, useEffect, useState } from "react";
import { api, normalizeError } from "../api";
import { formatBytes } from "../format";
import { store } from "../hooks";
import { useT, type Key } from "../i18n";
import type { ToolStatus, VideoTools } from "../types";
import { Panel } from "../ui/primitives";
import { Rich } from "../ui/rich";

// Shown by what each one is for, never by the program's name.
const TOOLS: { key: keyof VideoTools; name: Key; what: Key }[] = [
  { key: "ytdlp", name: "deps.sites", what: "deps.sitesWhat" },
  { key: "deno", name: "deps.checks", what: "deps.checksWhat" },
  { key: "ffmpeg", name: "deps.join", what: "deps.joinWhat" },
];

function state(s: ToolStatus, t: ReturnType<typeof useT>): { text: string; tone: string } {
  if (s.installed) return { text: t("deps.ready"), tone: "text-ok" };
  if (s.fetching && s.total > 0)
    return {
      text: t("deps.downloadingPct", { pct: Math.floor((s.received / s.total) * 100), got: formatBytes(s.received), total: formatBytes(s.total) }),
      tone: "text-dim",
    };
  if (s.fetching) return { text: s.received > 0 ? t("deps.downloadingBytes", { got: formatBytes(s.received) }) : t("deps.downloading"), tone: "text-dim" };
  if (s.error) return { text: t("deps.failed"), tone: "text-bad" };
  if (!s.canFetch) return { text: t("deps.system"), tone: "text-dim" };
  return { text: t("deps.waiting"), tone: "text-dim" };
}

/**
 * Settings > Browser > Required dependencies: the free programs VeloxFetch fetches by itself for video sites
 * (from official servers, checked against their published checksums). Nothing for the user to install, and
 * no program names on screen: each is shown by what it does.
 */
export function VideoToolsSettings() {
  const t = useT();
  const [tools, setTools] = useState<VideoTools | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setTools(await api.getVideoTools());
    } catch (e) {
      store.toast("error", normalizeError(e).message);
    }
  }, []);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => document.visibilityState === "visible" && void refresh(), 2000);
    return () => clearInterval(timer);
  }, [refresh]);

  const fetchNow = async () => {
    setBusy(true);
    try {
      const next = await api.fetchVideoTools();
      setTools(next);
      const missing = TOOLS.some((x) => !next[x.key].installed && next[x.key].canFetch);
      store.toast(missing ? "error" : "info", missing ? t("deps.someFailed") : t("deps.allReady"));
    } catch (e) {
      store.toast("error", normalizeError(e).message);
    } finally {
      setBusy(false);
    }
  };

  const allReady = !!tools && TOOLS.every((x) => tools[x.key].installed || !tools[x.key].canFetch);
  return (
    <Panel title={t("deps.title")} hint={t("deps.hint")}>
      <ul className="space-y-2" data-testid="video-tools">
        {TOOLS.map((x) => {
          const s = tools?.[x.key];
          const st = s ? state(s, t) : { text: "…", tone: "text-dim" };
          return (
            <li key={x.key} className="flex items-start justify-between gap-3 text-[13px]" data-testid={`tool-${x.key}`}>
              <span>
                <b>{t(x.name)}</b> <span className="text-dim">{t(x.what)}</span>
                {s?.error && !s.installed && <span className="mt-0.5 block text-xs text-bad">{t("deps.failedWhy")}</span>}
              </span>
              {/* At most about half the row, wrapping (long languages): never wider than the panel. */}
              <span className={`max-w-[45%] shrink-0 text-end text-xs ${st.tone}`}>{st.text}</span>
            </li>
          );
        })}
      </ul>
      {!allReady && (
        <button className="btn btn-primary mt-3" data-testid="video-tools-fetch" disabled={busy || !tools} onClick={() => void fetchNow()}>
          {busy ? t("deps.downloading") : t("deps.fetch")}
        </button>
      )}
      <p className="mt-2 text-xs text-dim">
        <Rich text={t("deps.trust")} />
      </p>
    </Panel>
  );
}
