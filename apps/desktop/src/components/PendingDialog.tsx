import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import { formatBytes } from "../format";
import { act, useStore } from "../hooks";
import { useT } from "../i18n";
import { IntelligencePanel } from "./IntelligencePanel";
import { NewLinkResult } from "./NewLinkDialog";
import { continuable } from "../newLink";
import { MediaDialog } from "./MediaDialog";
import type { NewLinkOutcome, PendingDownload, PickedFolder } from "../types";
import { Icon } from "../ui/icons";

function Row({ label, children, testid }: { label: string; children: React.ReactNode; testid?: string }) {
  return (
    <div className="flex gap-3 py-1.5 text-[13px]">
      <dt className="w-24 shrink-0 text-dim">{label}</dt>
      <dd className="selectable min-w-0 flex-1 break-all" data-testid={testid}>
        {children}
      </dd>
    </div>
  );
}

/** One waiting download: what it is, and Start / Later / Cancel. */
function Card({ p, waiting }: { p: PendingDownload; waiting: number }) {
  const t = useT();
  const [name, setName] = useState(p.filename);
  const edited = useRef(false);
  const start = useRef<HTMLButtonElement>(null);
  const [busy, setBusy] = useState(false);
  // A folder chosen for this one download (a token from the native folder dialog; the path is only shown).
  const [folder, setFolder] = useState<PickedFolder | null>(null);

  // Where the file goes under the name typed (sorting by type can move it to another folder).
  const [renamedDir, setRenamedDir] = useState<string | null>(null);
  useEffect(() => {
    const typed = name.trim();
    if (!typed || typed === p.filename) return setRenamedDir(null);
    let live = true;
    const timer = setTimeout(() => {
      api.previewPendingFolder(p.token, typed).then(
        (dir) => live && setRenamedDir(dir),
        () => live && setRenamedDir(null),
      );
    }, 250);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [name, p.filename, p.token, p.contentType]);

  // The server may tell us the real file name a moment later; keep it unless the user typed one.
  useEffect(() => {
    if (!edited.current) setName(p.filename);
  }, [p.filename]);
  useEffect(() => start.current?.focus(), []);

  const confirm = async (startNow: boolean) => {
    setBusy(true);
    const renamed = name.trim() && name.trim() !== p.filename ? name.trim() : undefined;
    await act(() => api.confirmPendingDownload(p.token, renamed, startNow, folder?.token), startNow ? undefined : t("pend.added"));
    setBusy(false);
  };
  const change = async () => {
    setBusy(true);
    const picked = await act(() => api.pickFolder());
    if (picked) setFolder(picked);
    setBusy(false);
  };
  const cancel = async () => {
    setBusy(true);
    await act(() => api.dismissPendingDownload(p.token));
    setBusy(false);
  };
  // A stopped download of the same file is in the list: this link (with the browser's session) may continue it.
  const target = continuable(p.analysis?.duplicate);
  const [relink, setRelink] = useState<NewLinkOutcome | null>(null);
  const [checking, setChecking] = useState(false);
  const continueStopped = async (mode: "auto" | "confirm") => {
    if (target === null) return;
    setBusy(true);
    setChecking(true);
    const o = await act(() => api.useNewLink(target, { pending: p.token }, mode));
    setChecking(false);
    setBusy(false);
    if (o) setRelink(o); // applied: the request is answered and this dialog goes away
  };

  useEffect(() => {
    const esc = (e: KeyboardEvent) => e.key === "Escape" && !busy && void cancel();
    document.addEventListener("keydown", esc);
    return () => document.removeEventListener("keydown", esc);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [busy, p.token]);

  return (
    <div className="card rise max-h-[92vh] w-full max-w-lg overflow-y-auto p-5 shadow-xl" role="dialog" aria-modal="true" aria-labelledby="pending-title" data-testid="pending-dialog">
      <div className="flex items-center gap-3">
        <div className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-accent-soft text-accent">
          <Icon name="download" size={19} />
        </div>
        <div className="min-w-0 flex-1">
          <h2 id="pending-title" className="text-[15px] font-semibold">
            {t("pend.title")}
          </h2>
          <p className="text-xs text-dim" data-testid="pending-count">
            {waiting > 1 ? t("pend.waiting", { n: waiting }) : t("pend.ask")}
          </p>
        </div>
      </div>

      <label className="mt-4 block text-xs font-medium text-dim" htmlFor="pending-name">
        {t("pend.saveAs")}
      </label>
      <input
        id="pending-name"
        data-testid="pending-name"
        className="input mt-1 w-full"
        value={name}
        maxLength={200}
        spellCheck={false}
        onChange={(e) => {
          edited.current = true;
          setName(e.target.value);
        }}
        onKeyDown={(e) => e.key === "Enter" && !busy && name.trim() && void confirm(true)}
      />

      <dl className="mt-3 divide-y divide-line border-y border-line">
        <Row label={t("pend.from")} testid="pending-host">
          {p.host}
        </Row>
        <Row label={t("pend.address")} testid="pending-url">
          {p.url}
        </Row>
        <Row label={t("pend.size")} testid="pending-size">
          {p.probing ? t("pend.checking") : p.size !== null ? formatBytes(p.size) : t("pend.unknown")}
        </Row>
        <Row label={t("pend.type")} testid="pending-type">
          {p.probing ? t("pend.checking") : (p.contentType ?? t("pend.unknown"))}
        </Row>
        <Row label={t("pend.resume")} testid="pending-resume">
          {p.probing
            ? t("pend.checking")
            : p.analysis
              ? t(`intel.resume.${p.analysis.resume}`)
              : p.resumable === null
                ? t("pend.unknown")
                : p.resumable
                  ? t("pend.resumeYes")
                  : t("pend.resumeNo")}
        </Row>
        <Row label={t("addd.saveIn")} testid="pending-dir">
          <div className="flex items-start gap-2">
            <span className="min-w-0 flex-1" data-testid="pending-dir-path">
              {folder ? folder.path : (renamedDir ?? p.saveDir)}
            </span>
            <button className="btn h-7 shrink-0 px-2 text-xs" data-testid="pending-change-folder" disabled={busy} onClick={() => void change()} title={t("pend.otherFolder")}>
              {t("store.change")}
            </button>
            {folder && (
              <button className="btn h-7 shrink-0 px-2 text-xs" data-testid="pending-default-folder" disabled={busy} onClick={() => setFolder(null)} title={t("pend.backUsual")}>
                {t("addd.usual")}
              </button>
            )}
          </div>
        </Row>
      </dl>
      {p.analysis && <IntelligencePanel a={p.analysis} />}
      {target !== null && (
        <div className="mt-3 space-y-2 rounded-lg border border-line px-3 py-2 text-xs" data-testid="pending-relink">
          <p>{t("nl.pendingAsk")}</p>
          {checking && <p className="text-dim" role="status">{t("nl.checking")}</p>}
          {relink && <NewLinkResult o={relink} />}
          <div className="flex justify-end">
            {relink?.canConfirm ? (
              <button className="btn h-7 px-2 text-xs" data-testid="pending-relink-confirm" disabled={busy} onClick={() => void continueStopped("confirm")}>
                {t("nl.confirm")}
              </button>
            ) : (
              !relink && (
                <button className="btn h-7 px-2 text-xs" data-testid="pending-relink-use" disabled={busy} onClick={() => void continueStopped("auto")}>
                  <Icon name="link" size={13} />
                  {t("nl.pendingUse")}
                </button>
              )
            )}
          </div>
        </div>
      )}
      {p.incognito && <p className="mt-2 text-xs text-dim">{t("pend.private")}</p>}

      <div className="mt-4 flex items-center justify-end gap-2">
        <button className="btn" data-testid="pending-cancel" disabled={busy} onClick={() => void cancel()}>
          {t("common.cancel")}
        </button>
        <button className="btn" data-testid="pending-later" disabled={busy || !name.trim()} onClick={() => void confirm(false)}>
          {t("pend.later")}
        </button>
        <button ref={start} className="btn btn-primary" data-testid="pending-start" disabled={busy || !name.trim()} onClick={() => void confirm(true)}>
          {t("pend.start")}
        </button>
      </div>
    </div>
  );
}

/** The confirmation popup for downloads handed over by the browser extension. */
export function PendingDialog() {
  const pending = useStore((s) => s.pending);
  const first = pending[0];
  if (!first) return null;
  // A video from the browser's "Download video" bar: pick the quality in the video dialog. Closing it
  // without starting answers the browser's request with "no".
  if (first.media) {
    return <MediaDialog key={first.token} initialUrl={first.url} pending={first.token} onClose={() => void api.dismissPendingDownload(first.token).catch(() => {})} />;
  }
  return (
    <div className="fixed inset-0 z-40 grid place-items-center bg-black/45 p-4" data-testid="pending-overlay">
      <Card key={first.token} p={first} waiting={pending.length} />
    </div>
  );
}
