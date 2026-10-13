import { useEffect, useState } from "react";
import { actions } from "../actions";
import { api, normalizeError } from "../api";
import { store } from "../hooks";
import { formatBytes, formatDuration, formatHttpStatus, formatSpeed } from "../format";
import type { Diagnostics, DownloadStatus } from "../types";
import { useT } from "../i18n";
import { Icon } from "../ui/icons";
import { withoutToolNames } from "./DiagnosticsSettings";
import { localEvent } from "../timeline";
import { networkLabel } from "../networks";

const RUNNING: DownloadStatus[] = ["queued", "downloading", "verifying"];

function Fact({ label, value, testId, tone }: { label: string; value: string; testId: string; tone?: "bad" | "ok" }) {
  return (
    <div className="rounded-xl border border-line bg-raised px-3 py-2.5">
      <div className="label">{label}</div>
      <div className={`num mt-0.5 truncate text-[13px] font-semibold ${tone === "bad" ? "text-bad" : tone === "ok" ? "text-ok" : ""}`} data-testid={testId} title={value}>
        {value}
      </div>
    </div>
  );
}

const DOT = { info: "bg-dim", warn: "bg-warn", error: "bg-bad" } as const;

/**
 * What happened to this download, over every attempt: the numbers support asks for first (HTTP status, error code,
 * retries, connections, average speed) and a short timeline. "Copy" puts a text version on the clipboard; it is
 * built in Rust with passwords, cookies, tokens and the link's query values already removed.
 */
export function DiagnosticsCard({ id, status, revision }: { id: number; status: DownloadStatus; revision?: unknown }) {
  const t = useT();
  const [diag, setDiag] = useState<Diagnostics | null>(null);
  const [error, setError] = useState<string | null>(null);
  const running = RUNNING.includes(status);

  useEffect(() => {
    let alive = true;
    const load = () =>
      api.getDownloadDiagnostics(id).then(
        (d) => alive && (setDiag(d), setError(null)),
        (e) => alive && setError(normalizeError(e).message),
      );
    void load();
    // Refresh while it runs; once it stops, the status change above reloads it one last time.
    const timer = running ? setInterval(load, 1000) : undefined;
    return () => {
      alive = false;
      if (timer) clearInterval(timer);
    };
  }, [id, status, running]);

  // A retry that fails at once can go failed -> failed between two renders (no status change to see): any
  // change to the stopped download reloads the card, shortly after, so the new attempt is counted.
  useEffect(() => {
    if (running || revision === undefined) return;
    let alive = true;
    const timer = setTimeout(
      () =>
        void api.getDownloadDiagnostics(id).then(
          (d) => alive && setDiag(d),
          () => {},
        ),
      150,
    );
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [id, running, revision]);

  const copy = async () => {
    if (!diag) return;
    const ok = await actions.copy(diag.report);
    store.toast(ok ? "info" : "error", ok ? t("dc.copied") : t("diag.noClipboard"));
  };

  const http = diag?.httpStatus ?? null;
  return (
    <section className="card p-4" data-testid="diagnostics-card">
      <div className="mb-3 flex items-center justify-between gap-3">
        <div>
          <h2 className="text-[13px] font-semibold">{t("diag.title")}</h2>
          <p className="text-xs text-dim">{t("dc.sub")}</p>
        </div>
        <button className="btn" data-testid="diagnostics-copy" disabled={!diag} onClick={() => void copy()}>
          <Icon name="copy" size={14} /> {t("dc.copy")}
        </button>
      </div>
      {!diag ? (
        <p className="text-xs text-dim">{error ?? t("common.loading")}</p>
      ) : (
        <>
          <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
            <Fact label={t("dc.http")} value={formatHttpStatus(http)} testId="diag-http" tone={http === null ? undefined : http >= 400 ? "bad" : "ok"} />
            <Fact label={t("dc.code")} value={diag.errorCode ?? t("common.none")} testId="diag-code" tone={diag.errorCode ? "bad" : undefined} />
            <Fact label={t("dc.attempts")} value={String(diag.attempts)} testId="diag-attempts" />
            <Fact label={t("dc.retries")} value={String(diag.retries)} testId="diag-retries" />
            <Fact label={t("dc.conn")} value={t("dc.connVal", { now: diag.connections, max: diag.peakConnections })} testId="diag-connections" />
            <Fact label={t("dc.avg")} value={diag.avgSpeedBps > 0 ? formatSpeed(diag.avgSpeedBps) : "—"} testId="diag-speed" />
            <Fact label={t("dc.time")} value={diag.activeSecs > 0 ? formatDuration(diag.activeSecs) : "—"} testId="diag-time" />
            <Fact label={t("dc.received")} value={formatBytes(diag.transferred)} testId="diag-received" />
          </div>
          {diag.networks.length > 0 && (
            <p className="mt-2 text-xs text-dim" data-testid="diag-networks">
              <span className="font-medium text-fg">{t("dc.networks")}</span>{" "}
              {diag.networks.map((n) => `${networkLabel(n, diag.networks, t)} ${formatBytes(n.bytes)}`).join(" · ")}
            </p>
          )}
          {diag.events.length > 0 && (
            <ol className="selectable mt-3 max-h-56 space-y-1 overflow-y-auto rounded-xl border border-line bg-raised p-2.5 text-xs" data-testid="diag-events" aria-label={t("dc.timeline")}>
              {diag.events.map((e, i) => (
                <li key={`${e.at}-${i}`} className="flex items-start gap-2">
                  <span className={`mt-1.5 size-1.5 shrink-0 rounded-full ${DOT[e.level]}`} aria-hidden />
                  <time className="num shrink-0 text-dim" dateTime={new Date(e.at).toISOString()}>
                    {new Date(e.at).toLocaleTimeString(document.documentElement.lang || undefined)}
                  </time>
                  <span className="min-w-0 break-words">{withoutToolNames(localEvent(e.text))}</span>
                </li>
              ))}
            </ol>
          )}
        </>
      )}
    </section>
  );
}
