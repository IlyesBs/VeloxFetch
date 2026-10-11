import { localError } from "../errors";
import { memo, useCallback, useState, type MouseEvent } from "react";
import { actions } from "../actions";
import { formatBytes, formatEta, formatSpeed, fraction, hostOf, percent } from "../format";
import { useDownload } from "../hooks";
import { t, useT } from "../i18n";
import type { Density } from "../prefs";
import type { Download, FileKind } from "../types";
import { Icon, type IconName } from "../ui/icons";
import { ContextMenu, Menu, ProgressBar, StatusChip, type MenuItem } from "../ui/primitives";
import { canRedownload, redownloadIsPrimary } from "../redownload";
import { canUseNewLink, openNewLink } from "../newLink";
import { canOpen, kindLabel } from "./FileIcon";
import { DeleteConfirm } from "./DeleteConfirm";
import { SignInDialog } from "./SignInDialog";
import { ThreadMatrix } from "./ThreadMatrix";

/** The one primary button a download offers in its current state. `round`: the card's circular style. */
export function PrimaryAction({ d, compact = false, round = false, onSignIn }: { d: Download; compact?: boolean; round?: boolean; onSignIn?: () => void }) {
  const t = useT();
  const btn = (label: string, icon: "pause" | "play" | "retry" | "lock" | "external" | "download", onClick: () => void, primary = false, testId?: string) => (
    <button
      className={round ? `round-btn ${primary ? "round-primary" : ""}` : `btn ${compact ? "btn-icon" : ""} ${primary ? "btn-primary" : ""}`}
      aria-label={label}
      title={label}
      data-testid={testId}
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
    >
      <Icon name={round && icon === "external" ? "play" : icon} size={round ? 17 : 15} />
      {!compact && !round && label}
    </button>
  );
  if (redownloadIsPrimary(d)) return btn(t("card.redownload"), "download", () => void actions.redownload(d), true, "card-redownload");
  switch (d.status) {
    case "queued":
      // Added without starting ("Download later"): only the user starts it.
      if (d.idle) return btn(t("card.start"), "play", () => void actions.start(d.id), true, "card-start");
      return btn(t("card.pause"), "pause", () => void actions.pause(d.id));
    case "downloading":
      return btn(t("card.pause"), "pause", () => void actions.pause(d.id));
    case "paused":
      return btn(t("card.resume"), "play", () => void actions.resume(d.id), true);
    case "failed":
      return d.needs === "signIn" && onSignIn ? btn(t("card.signIn"), "lock", onSignIn, true, "signin-open") : btn(t("card.retry"), "retry", () => void actions.retry(d.id), true);
    case "completed":
      return canOpen(d.filename, d.kind) ? btn(t("card.open"), "external", () => void actions.openFile({ kind: "download", id: d.id }), false, "open-file") : null;
    default:
      return null;
  }
}

/** The right-click / "..." menu of a download (labels in the current language; callers re-render on a change). */
export function downloadMenu(d: Download, onOpen: (id: number) => void, onSignIn?: () => void, onDelete?: () => void): MenuItem[] {
  const cancellable = !["completed", "cancelled"].includes(d.status);
  const target = { kind: "download", id: d.id } as const;
  return [
    ...(d.status === "queued" && d.idle ? [{ label: t("card.start"), icon: "play" as const, onSelect: () => void actions.start(d.id) }] : []),
    ...(d.status === "completed" && !d.fileMissing && canOpen(d.filename, d.kind) ? [{ label: t("card.open"), icon: "external" as const, onSelect: () => void actions.openFile(target) }] : []),
    ...(d.status !== "cancelled" ? [{ label: t("card.showFolder"), icon: "folder" as const, onSelect: () => void actions.showInFolder(target) }] : []),
    ...(d.status === "failed" && onSignIn ? [{ label: t("card.signInRetry"), icon: "lock" as const, onSelect: onSignIn }] : []),
    ...(canUseNewLink(d) ? [{ label: t("nl.menu"), icon: "link" as const, onSelect: () => openNewLink(d.id) }] : []),
    ...(canRedownload(d) ? [{ label: t("card.redownload"), icon: "download" as const, onSelect: () => void actions.redownload(d) }] : []),
    { label: t("card.details"), icon: "layers", onSelect: () => onOpen(d.id) },
    { label: t("card.copy"), icon: "copy", onSelect: () => void actions.copy(d.url) },
    { label: t("card.cancel"), icon: "x", disabled: !cancellable, onSelect: () => void actions.cancel(d.id) },
    { label: t("card.remove"), icon: "trash", danger: true, onSelect: () => void actions.remove(d.id, false) },
    ...(d.status === "completed"
      ? [{ label: t("card.removeDelete"), icon: "trash" as const, danger: true, onSelect: onDelete ?? (() => void actions.remove(d.id, true)) }]
      : []),
  ];
}

const KIND_ICON: Record<FileKind, IconName> = {
  video: "film",
  audio: "music",
  image: "image",
  document: "doc",
  archive: "archive",
  program: "app",
  other: "file",
};

/** The card's picture: the kind of file, big, on its own colour, with the file's extension. */
function Thumb({ kind, filename }: { kind: FileKind; filename: string }) {
  const ext = /\.([a-z0-9]{1,5})$/i.exec(filename)?.[1];
  return (
    <div className={`thumb kind-${kind}`} title={kindLabel(kind)} data-kind={kind}>
      <Icon name={KIND_ICON[kind]} size={40} strokeWidth={1.4} />
      {ext && <span className="thumb-ext">{ext}</span>}
    </div>
  );
}

/**
 * A download as a task card (the reference layout): its picture, name, size and link, a neon progress
 * track with the percent, live numbers, the 8x8 connection panel and round buttons. Subscribes to its
 * own download only and is memoised, so a tick that didn't change this download doesn't re-render it.
 * `compact` drops the picture and the panel for long lists.
 */
export const DownloadCard = memo(function DownloadCard({ id, onOpen, density = "cards" }: { id: number; onOpen: (id: number) => void; density?: Density }) {
  const t = useT();
  const d = useDownload(id);
  const [menuAt, setMenuAt] = useState<{ x: number; y: number } | null>(null);
  const [signIn, setSignIn] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const closeMenu = useCallback(() => setMenuAt(null), []);
  const closeSignIn = useCallback(() => setSignIn(false), []);
  const closeConfirm = useCallback(() => setConfirm(false), []);
  if (!d) return null;
  const done = d.status === "completed";
  const frac = done ? 1 : fraction(d.downloaded, d.totalSize);
  const running = d.status === "downloading";
  const tone = done ? "ok" : d.status === "paused" ? "warn" : d.status === "failed" ? "bad" : "accent";
  const openSignIn = () => setSignIn(true);
  const menu = downloadMenu(d, onOpen, openSignIn, () => setConfirm(true));
  const contextMenu = (e: MouseEvent) => {
    e.preventDefault();
    setMenuAt({ x: e.clientX, y: e.clientY });
  };
  const pct = done ? 100 : d.totalSize ? percent(d.downloaded, d.totalSize) : null;
  const size = d.totalSize !== null ? formatBytes(d.totalSize) : done ? formatBytes(d.downloaded) : t("card.unknownSize");
  const cancellable = !["completed", "cancelled", "failed"].includes(d.status);

  return (
    <article
      className={`task @container rise cursor-pointer ${density === "compact" ? "task-compact px-4 py-3" : "p-4"} ${running ? "is-live" : ""}`}
      data-testid={`download-${d.id}`}
      data-status={d.status}
      onClick={() => onOpen(d.id)}
      onContextMenu={contextMenu}
    >
      {menuAt && <ContextMenu at={menuAt} items={menu} onClose={closeMenu} />}
      {signIn && <SignInDialog download={d} onClose={closeSignIn} />}
      {confirm && (
        <DeleteConfirm
          name={d.filename}
          onDelete={() => void actions.deleteFile(d)}
          onRemove={() => void actions.remove(d.id, false)}
          onClose={closeConfirm}
        />
      )}
      <div className="task-grid">
        <div className="task-thumb">
          <Thumb kind={d.kind} filename={d.filename} />
        </div>

        <div className="task-body">
          <button
            className="block max-w-full truncate text-start text-[15px] font-semibold hover:underline"
            onClick={(e) => {
              e.stopPropagation();
              onOpen(d.id);
            }}
            title={d.filename}
          >
            {d.filename}
          </button>
          <p className="mt-0.5 flex min-w-0 items-center gap-1.5 text-[12.5px] text-dim">
            <span className="num ltr shrink-0">{size}</span>
            <span aria-hidden="true">•</span>
            <span className="ltr truncate" title={d.url}>
              {d.url}
            </span>
          </p>

          <div className="mt-3 flex items-center gap-4">
            <ProgressBar value={frac} tone={tone} label={t("card.progress", { name: d.filename })} live={running} thick className="min-w-0 flex-1" />
            <span className={`num display ltr w-[4.2rem] shrink-0 text-end text-[18px] font-semibold leading-none ${done ? "text-ok" : running ? "text-fg" : "text-fg/80"}`} data-testid="percent">
              {pct !== null ? (done ? "100%" : `${pct.toFixed(1)}%`) : "—"}
            </span>
          </div>

          <div className="mt-2.5 flex flex-wrap items-center gap-x-5 gap-y-1.5 text-[12.5px] text-dim">
            {running && (
              <>
                <span className="num flex items-center gap-1.5 font-semibold text-accent" data-testid="speed">
                  <Icon name="download" size={14} />
                  <span className="ltr">{formatSpeed(d.speedBps)}</span>
                </span>
                <span className="num flex items-center gap-1.5" title={t("card.eta", { t: formatEta(d.etaSecs) })}>
                  <Icon name="clock" size={14} />
                  <span className="ltr">{formatEta(d.etaSecs)}</span>
                </span>
              </>
            )}
            {done && (
              <span className="flex items-center gap-1.5 font-medium text-ok">
                <Icon name="checkCircle" size={15} />
                {t("status.completed")}
              </span>
            )}
            {!running && !done && <StatusChip status={d.status} />}
            {d.status === "queued" && <span>{t(d.idle ? "card.notStarted" : "card.waitingSlot")}</span>}
            <span className="num ltr">
              {formatBytes(done ? (d.totalSize ?? d.downloaded) : d.downloaded)}
              {d.totalSize !== null ? ` / ${formatBytes(d.totalSize)}` : ""}
            </span>
            {running && <span className="num hidden sm:inline">{t("card.conn", { n: d.connections })}</span>}
            {done && <span className="truncate">{d.category ?? hostOf(d.url)}</span>}
          </div>

          {d.fileMissing && (
            <p className="mt-2.5 flex items-center gap-1.5 text-xs text-warn" data-testid="card-file-missing">
              <Icon name="alert" size={14} className="shrink-0" />
              {t("card.fileMissing")}
            </p>
          )}

          {d.error && (
            <p className="mt-2.5 flex items-start gap-1.5 rounded-lg bg-bad/10 px-2.5 py-1.5 text-xs text-bad" role="alert" title={d.errorDetail ?? undefined} data-testid="card-error">
              <Icon name="alert" size={14} className="mt-px shrink-0" />
              {localError(d.errorCode, d.error)}
            </p>
          )}
        </div>

        <div className="task-threads">
          <ThreadMatrix panel connections={running ? d.connections : 0} speedBps={d.speedBps} status={d.status} />
        </div>

        <div className="task-actions flex items-center gap-1.5" onClick={(e) => e.stopPropagation()}>
          <PrimaryAction d={d} round onSignIn={openSignIn} />
          {cancellable && (
            <button className="round-btn" aria-label={t("card.cancel")} title={t("card.cancel")} data-testid="card-cancel" onClick={() => void actions.cancel(d.id)}>
              <Icon name="x" size={17} />
            </button>
          )}
          {d.status !== "cancelled" && (
            <button className="round-btn" aria-label={t("card.showFolder")} title={t("card.showFolder")} data-testid="show-in-folder" onClick={() => void actions.showInFolder({ kind: "download", id: d.id })}>
              <Icon name="folder" size={17} />
            </button>
          )}
          <Menu label={t("card.more", { name: d.filename })} items={menu} vertical />
        </div>
      </div>
    </article>
  );
});
