import { useCallback, useEffect, useState } from "react";
import { actions } from "../actions";
import { api, normalizeError } from "../api";
import { act, store } from "../hooks";
import { useT } from "../i18n";
import type { Logs } from "../types";
import { Icon } from "../ui/icons";
import { Panel } from "../ui/primitives";

/** On screen, the helper programs are not named (the copied report and the log files keep the names for support). */
export function withoutToolNames(text: string): string {
  return text.replace(/\b(yt-dlp|youtube-dl|deno|ffmpeg|ffprobe)\b/gi, "dependency");
}

const LEVEL_TONE: Record<string, string> = { ERROR: "text-bad", WARN: "text-warn", INFO: "text-dim", DEBUG: "text-dim/70", TRACE: "text-dim/70" };

/**
 * Settings > Diagnostics: detailed logging, the recent log, a report to paste in a bug report, the log folder.
 * Everything is cleaned of secrets in Rust before it gets here; nothing is ever sent anywhere.
 */
export function DiagnosticsSettings() {
  const t = useT();
  const [logs, setLogs] = useState<Logs | null>(null);
  const [problemsOnly, setProblemsOnly] = useState(false);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setLogs(await api.getLogs(300, problemsOnly));
    } catch (e) {
      store.toast("error", normalizeError(e).message);
    }
  }, [problemsOnly]);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => document.visibilityState === "visible" && void refresh(), 3000);
    return () => clearInterval(timer);
  }, [refresh]);

  const toggleDetailed = async (enabled: boolean) => {
    await act(() => api.setDetailedLogging(enabled), enabled ? t("diag.detailedOn") : t("diag.detailedOff"));
    void refresh(); // shows the real state either way
  };

  const copyReport = async () => {
    setBusy(true);
    try {
      const report = await api.getDiagnosticsReport();
      const ok = await actions.copy(report);
      store.toast(ok ? "info" : "error", ok ? t("diag.copied") : t("diag.noClipboard"));
    } catch (e) {
      store.toast("error", normalizeError(e).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Panel title={t("diag.title")} hint={t("diag.hint")}>
      <label className="flex cursor-pointer items-center gap-2 text-[13px]">
        <input type="checkbox" data-testid="detailed-logging" checked={logs?.detailed ?? false} disabled={!logs} onChange={(e) => void toggleDetailed(e.target.checked)} />
        {t("diag.detailed")}
      </label>
      <p className="mt-1 text-xs text-dim">
        {t("diag.detailedHow")}
      </p>

      <div className="mt-4 flex flex-wrap gap-2">
        <button className="btn btn-primary" data-testid="copy-report" disabled={busy} onClick={() => void copyReport()}>
          <Icon name="copy" size={14} /> {t("diag.copy")}
        </button>
        <button className="btn" data-testid="open-log-folder" disabled={!logs?.fileLogging} onClick={() => void act(() => api.openLogFolder())}>
          <Icon name="folder" size={14} /> {t("diag.openFolder")}
        </button>
      </div>

      <div className="mt-5 flex items-center justify-between gap-3">
        <span className="label">{t("diag.recent")}</span>
        <div className="flex items-center gap-2">
          <div role="radiogroup" aria-label={t("diag.which")} className="inline-flex rounded-lg border border-line bg-raised p-0.5">
            {[
              { v: false, id: "all", label: t("diag.all") },
              { v: true, id: "problems", label: t("diag.problems") },
            ].map((o) => (
              <button
                key={o.id}
                role="radio"
                aria-checked={problemsOnly === o.v}
                data-testid={`log-filter-${o.id}`}
                onClick={() => setProblemsOnly(o.v)}
                className={`h-7 rounded-md px-2.5 text-xs font-medium ${problemsOnly === o.v ? "bg-surface text-fg shadow-sm" : "text-dim hover:text-fg"}`}
              >
                {o.label}
              </button>
            ))}
          </div>
          <button className="btn btn-ghost btn-icon" aria-label={t("diag.refresh")} onClick={() => void refresh()}>
            <Icon name="retry" size={14} />
          </button>
        </div>
      </div>
      <ol className="selectable mt-2 max-h-72 overflow-y-auto rounded-xl border border-line bg-raised p-2 font-mono text-[11px] leading-relaxed" data-testid="log-lines" aria-label={t("diag.log")}>
        {logs && logs.lines.length === 0 && <li className="px-1 py-2 text-dim">{problemsOnly ? t("diag.noProblems") : t("diag.empty")}</li>}
        {logs?.lines
          .slice()
          .reverse()
          .map((l, i) => (
            <li key={`${l.at}-${i}`} className="flex gap-2 px-1 py-0.5">
              <time className="shrink-0 text-dim" dateTime={new Date(l.at).toISOString()}>
                {new Date(l.at).toLocaleTimeString(document.documentElement.lang || undefined)}
              </time>
              <span className={`w-11 shrink-0 font-semibold ${LEVEL_TONE[l.level] ?? ""}`}>{l.level}</span>
              <span className="min-w-0 [overflow-wrap:anywhere]">{withoutToolNames(l.text)}</span>
            </li>
          ))}
      </ol>
      <p className="mt-2 text-xs text-dim">{t("diag.newest")} {logs?.fileLogging ? t("diag.files") : ""}</p>
    </Panel>
  );
}
