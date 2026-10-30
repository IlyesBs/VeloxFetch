import { useEffect, useRef, useState, type FormEvent } from "react";
import { createPortal } from "react-dom";
import { api } from "../api";
import { opensVideoDialog, parseRate } from "../format";
import { act, store, useStore } from "../hooks";
import { PRIORITIES, type Analysis, type DuplicateMatch, type FileKind, type NewDownload, type PickedFolder, type Priority, type Storage } from "../types";
import { RememberFolder, RememberedNote, rememberedFor } from "./RememberFolder";
import { t as tr, useT, type Key } from "../i18n";
import { Icon } from "../ui/icons";
import { StatusChip } from "../ui/primitives";
import { localStamp, MAIN_QUEUE, queueName } from "../smartQueue";
import { IntelligencePanel } from "./IntelligencePanel";
import { actions } from "../actions";
import { duplicatePrompt, localDuplicate, type DupAction } from "../duplicates";

/** Put `url` in the link bar and focus it (paste / drop anywhere, Ctrl+L). An empty string only focuses. */
export const FILL_LINK = "velox:fill-link";
/** Open the Add dialog with its options (Ctrl+N). */
export const OPEN_ADD = "velox:open-add";

const EMPTY_ADVANCED = { username: "", password: "", referrer: "", cookies: "", userAgent: "" };

const DUP_LABEL: Record<DupAction, Key> = {
  view: "dup.show",
  resume: "dup.resume",
  retry: "dup.retry",
  open: "dup.open",
  folder: "dup.folder",
  again: "dup.again",
  another: "dup.another",
};

/** "You already have this": what the app knows of it, and only the actions that can work. Downloading anyway stays possible. */
function DuplicatePrompt({ d, onAction, onClose }: { d: DuplicateMatch; onAction: (a: DupAction) => void; onClose: () => void }) {
  const t = useT();
  const p = duplicatePrompt(d);
  useEffect(() => {
    const esc = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("keydown", esc);
    return () => document.removeEventListener("keydown", esc);
  }, [onClose]);
  return createPortal(
    <div className="fixed inset-0 z-50 grid place-items-center bg-black/45 p-4" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <div
        className="card rise w-full max-w-md space-y-4 p-5 shadow-xl"
        role="dialog"
        aria-modal="true"
        aria-labelledby="dup-title"
        data-testid="duplicate-dialog"
        data-confidence={d.confidence}
        data-file={d.file ?? ""}
      >
        <div className="flex items-center gap-3">
          <div className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-warn/15 text-warn">
            <Icon name="copy" size={19} />
          </div>
          <div className="min-w-0">
            <h2 id="dup-title" className="text-[15px] font-semibold" data-testid="duplicate-title">
              {t(p.title)}
            </h2>
            <p className="text-xs text-dim">{t(p.reason)}</p>
          </div>
        </div>
        <div className="flex items-center gap-2 rounded-xl border border-line bg-raised px-3 py-2">
          <span className="min-w-0 flex-1 truncate text-[13px] font-medium">{d.filename}</span>
          {d.status && <StatusChip status={d.status} />}
        </div>
        {p.notes.length > 0 && (
          <ul className="space-y-1 text-xs text-dim" data-testid="duplicate-notes">
            {p.notes.map((n) => (
              <li key={n} className={n === "dup.n.changed" ? "text-warn" : undefined}>
                {t(n)}
              </li>
            ))}
          </ul>
        )}
        <div className="flex flex-wrap justify-end gap-2">
          <button className="btn" onClick={onClose}>
            {t("common.cancel")}
          </button>
          {p.actions.map((a, i) => (
            <button key={a} className={i === 0 ? "btn btn-primary" : "btn"} data-testid={`duplicate-${a === "view" ? "show" : a}`} onClick={() => onAction(a)} autoFocus={i === 0}>
              {t(DUP_LABEL[a])}
            </button>
          ))}
        </div>
      </div>
    </div>,
    document.body,
  );
}

/** Quick "paste a link" bar plus an options dialog. All validation is repeated in Rust. */
export function AddDownload({ onBatch, onStream, onOpen }: { onBatch?: () => void; onStream?: (url: string) => void; onOpen?: (id: number) => void }) {
  const t = useT();
  const [url, setUrl] = useState("");
  const dialog = useRef<HTMLDialogElement>(null);
  const bar = useRef<HTMLInputElement>(null);
  const [opts, setOpts] = useState({ subdir: "", filename: "", priority: "normal" as Priority, limit: "", checksum: "" });
  // Smart Download Queue: which queue, and now / later / at a set time.
  // (select the stored object itself: a selector that builds a new array would re-render forever)
  const queueState = useStore((s) => s.queues);
  const queues = queueState?.queues ?? [];
  const [queueId, setQueueId] = useState(MAIN_QUEUE);
  const [when, setWhen] = useState<"now" | "later" | "at">("now");
  const [startAt, setStartAt] = useState("");
  const [adv, setAdv] = useState(EMPTY_ADVANCED);
  const [formError, setFormError] = useState<string | null>(null);
  // A folder chosen for this download in the native dialog (a token; the path is only shown).
  const [folder, setFolder] = useState<PickedFolder | null>(null);
  // "Remember this folder for <kind>" (only with a picked folder); the kind is the check's, or the one chosen.
  const [remember, setRemember] = useState(false);
  const [rememberKind, setRememberKind] = useState<FileKind | null>(null);
  const [storage, setStorage] = useState<Storage | null>(null);
  // A download the app already has for this link, waiting for the user's answer.
  const [dup, setDup] = useState<{ d: DuplicateMatch; req: NewDownload; fromDialog: boolean } | null>(null);
  // A duplicate check is running (a second press of Add waits for it instead of asking twice).
  const asking = useRef(false);
  // the list under the Add button's arrow (several links, a video, options)
  const [moreOpen, setMoreOpen] = useState(false);
  const more = useRef<HTMLDivElement>(null);
  // Download Intelligence in the options dialog: "Check link" looks at the link without downloading it.
  // A newer check, a changed link or closing the dialog drops (and cancels) the one running.
  const [check, setCheck] = useState<{ s: "idle" } | { s: "checking" } | { s: "failed" } | { s: "done"; a: Analysis }>({ s: "idle" });
  const checkSeq = useRef(0);
  const checking = useRef(false);
  const runCheck = async () => {
    const link = url.trim();
    if (!link) return;
    const seq = ++checkSeq.current;
    checking.current = true;
    setCheck({ s: "checking" });
    try {
      const a = await api.analyzeLink(link);
      if (seq === checkSeq.current) setCheck(a ? { s: "done", a } : { s: "idle" });
    } catch {
      if (seq === checkSeq.current) setCheck({ s: "failed" });
    } finally {
      if (seq === checkSeq.current) checking.current = false;
    }
  };
  const stopCheck = () => {
    checkSeq.current++;
    if (checking.current) void api.analyzeLink(null).catch(() => {});
    checking.current = false;
    setCheck({ s: "idle" });
  };

  const checkedKind = check.s === "done" ? check.a.fileKind : null;
  const kind = rememberKind ?? checkedKind;
  // The folders remembered per kind: what "the usual folder" means for this file.
  useEffect(() => {
    let live = true;
    api.getStorage().then(
      (s) => live && setStorage(s),
      () => {},
    );
    return () => {
      live = false;
    };
  }, [checkedKind, folder]);
  const remembered = rememberedFor(storage, checkedKind);
  const toggleRemember = (on: boolean) => {
    setRemember(on);
    // The kind decides which folder is remembered: look at the link to find it, unless it is known already.
    if (on && !kind && check.s === "idle") void runCheck();
  };

  useEffect(() => {
    if (!moreOpen) return;
    const close = () => setMoreOpen(false);
    const away = (e: Event) => !more.current?.contains(e.target as Node) && close();
    const esc = (e: KeyboardEvent) => e.key === "Escape" && close();
    document.addEventListener("pointerdown", away, true);
    document.addEventListener("keydown", esc);
    window.addEventListener("blur", close);
    return () => {
      document.removeEventListener("pointerdown", away, true);
      document.removeEventListener("keydown", esc);
      window.removeEventListener("blur", close);
    };
  }, [moreOpen]);

  /** An item of that list: close it, then act. */
  const pick = (fn: () => void) => () => {
    setMoreOpen(false);
    fn();
  };

  /** The paste button: the first http(s) link on the clipboard goes in the bar (nothing else is kept). */
  const paste = async () => {
    let text: string;
    try {
      text = await navigator.clipboard.readText();
    } catch {
      bar.current?.focus();
      store.toast("info", t("add.pasteHint"));
      return;
    }
    const link = /https?:\/\/[^\s<>"']+/i.exec(text)?.[0];
    if (!link) return store.toast("info", t("add.noLink"));
    setUrl(link);
    bar.current?.focus();
  };

  useEffect(() => {
    const fill = (e: Event) => {
      const link = (e as CustomEvent<string>).detail;
      if (link) setUrl(link);
      bar.current?.focus();
      if (link) bar.current?.select();
    };
    const open = () => dialog.current?.showModal();
    window.addEventListener(FILL_LINK, fill);
    window.addEventListener(OPEN_ADD, open);
    return () => {
      window.removeEventListener(FILL_LINK, fill);
      window.removeEventListener(OPEN_ADD, open);
    };
  }, []);

  const chooseFolder = async () => {
    const picked = await act(() => api.pickFolder());
    if (picked) setFolder(picked);
  };
  const usualFolder = () => {
    setFolder(null);
    setRemember(false);
  };

  const reset = () => {
    setUrl("");
    setFolder(null);
    setRemember(false);
    setRememberKind(null);
    setAdv(EMPTY_ADVANCED);
  };

  const create = async (req: NewDownload, fromDialog: boolean) => {
    const ok = await act(() => api.createDownload(req));
    if (ok !== undefined) {
      reset();
      if (fromDialog) dialog.current?.close();
    }
  };

  /** Ask first when the app already has this download (Rust looks at the list, the history and the finished file). */
  const submit = async (req: NewDownload, fromDialog: boolean) => {
    if (asking.current) return;
    asking.current = true;
    let d: DuplicateMatch | null;
    try {
      d = await api.checkDuplicate(req.url, req.checksum);
    } catch {
      // Rust could not be asked: a plain comparison with the list, so a repeat is still noticed.
      const { byId, ids } = store.getState();
      d = localDuplicate(req.url, byId, ids);
    } finally {
      asking.current = false;
    }
    if (d) return setDup({ d, req, fromDialog });
    await create(req, fromDialog);
  };

  /** The answer to the duplicate prompt. Nothing existing is ever removed, replaced or cancelled. */
  const onDuplicate = (a: DupAction) => {
    if (!dup) return;
    const { d, req, fromDialog } = dup;
    setDup(null);
    if (a === "again" || a === "another") return void create(req, fromDialog);
    reset();
    dialog.current?.close();
    const target = d.id !== null ? ({ kind: "download", id: d.id } as const) : d.historyId !== null ? ({ kind: "history", id: d.historyId } as const) : null;
    if (a === "resume" && d.id !== null) void actions.resume(d.id);
    if (a === "retry" && d.id !== null) void actions.retry(d.id);
    if (a === "open" && target) void actions.openFile(target);
    if (a === "folder" && target) void actions.showInFolder(target);
    if (a === "view" && d.id !== null) onOpen?.(d.id);
  };

  const quick = async (e: FormEvent) => {
    e.preventDefault();
    if (!url.trim()) return;
    // A .m3u8 / .mpd link or a video page (YouTube...) has its own dialog (quality, audio, subtitles).
    if (onStream && opensVideoDialog(url)) {
      onStream(url.trim());
      setUrl("");
      return;
    }
    await submit({ url: url.trim() }, false);
  };

  const detailed = async (e: FormEvent) => {
    e.preventDefault();
    setFormError(null);
    if (onStream && opensVideoDialog(url)) {
      onStream(url.trim());
      setUrl("");
      dialog.current?.close();
      return;
    }
    const limit = opts.limit.trim() ? parseRate(opts.limit) : null;
    if (opts.limit.trim() && limit === null) return setFormError(tr("addd.badLimit"));
    if (adv.password && !adv.username.trim()) return setFormError(tr("addd.passNeedsUser"));
    if (when === "at" && !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/.test(startAt)) return setFormError(tr("addd.badStartAt"));
    if (folder && remember && !kind) return setFormError(tr("folder.needKind"));
    const filled = (v: string) => v.trim() || undefined;
    const req: NewDownload = {
      url: url.trim(),
      priority: opts.priority,
      start: when === "now",
      ...(when === "at" && { startAt }),
      ...(queueId !== MAIN_QUEUE && queues.some((q) => q.id === queueId) && { queueId }),
      ...(folder ? { folderToken: folder.token } : opts.subdir.trim() && { subdir: opts.subdir.trim() }),
      ...(folder && remember && kind && { rememberFor: kind }),
      ...(opts.filename.trim() && { filename: opts.filename.trim() }),
      ...(limit && { speedLimit: limit }),
      ...(opts.checksum.trim() && { checksum: opts.checksum.trim() }),
      ...(filled(adv.username) && { username: adv.username.trim(), password: adv.password }),
      ...(filled(adv.referrer) && { referrer: adv.referrer.trim() }),
      ...(filled(adv.cookies) && { cookies: adv.cookies.trim() }),
      ...(filled(adv.userAgent) && { userAgent: adv.userAgent.trim() }),
    };
    await submit(req, true);
  };

  const advancedUsed = Object.values(adv).some((v) => v.trim() !== "");

  return (
    <>
      <form className="@container flex items-center gap-3" onSubmit={quick} data-testid="add-form">
        <div className="add-bar card relative flex h-[52px] min-w-0 flex-1 items-center rounded-[16px] pe-1.5">
          <Icon name="link" size={20} className="pointer-events-none absolute start-4 top-1/2 -translate-y-1/2 text-dim" />
          <input
            ref={bar}
            aria-label={t("add.url")}
            data-testid="url-input"
            className="input h-full min-w-0 flex-1 rounded-[16px] border-transparent bg-transparent ps-12 text-[15px] hover:border-transparent focus:border-transparent focus:shadow-none"
            placeholder={t("add.placeholder")}
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            spellCheck={false}
            autoComplete="off"
          />
          <button type="button" className="tool-btn" aria-label={t("add.paste")} title={t("add.paste")} data-testid="paste-link" onClick={() => void paste()}>
            <Icon name="clipboard" size={19} />
          </button>
        </div>
        <div className="relative" ref={more}>
          <div className="btn-neon">
            <button type="submit" className="ps-4 pe-4 @[520px]:ps-6" disabled={!url.trim()} data-testid="add-button" aria-label={t("add.download")}>
              <Icon name="plus" size={19} strokeWidth={2.2} />
              <span className="hidden @[520px]:inline">{t("add.download")}</span>
            </button>
            <button type="button" className="btn-neon-split" aria-label={t("add.more")} title={t("add.more")} aria-haspopup="true" aria-expanded={moreOpen} data-testid="add-more" onClick={() => setMoreOpen(!moreOpen)}>
              <Icon name="chevronDown" size={17} />
            </button>
          </div>
          {/* kept in the page while closed (hidden), so each way to add stays reachable by its id */}
          <div className="popover end-0 top-[3.4rem] w-60" hidden={!moreOpen} data-testid="add-more-list">
            {onBatch && (
              <button type="button" className="popover-item" onClick={pick(onBatch)} aria-haspopup="dialog" data-testid="batch-open" title={t("add.severalTip")}>
                <Icon name="layers" size={16} />
                {t("add.several")}
              </button>
            )}
            {onStream && (
              <button type="button" className="popover-item" onClick={pick(() => onStream(url.trim()))} aria-haspopup="dialog" data-testid="stream-open" title={t("add.videoTip")}>
                <Icon name="film" size={16} />
                {t("add.video")}
              </button>
            )}
            <button type="button" className="popover-item" onClick={pick(() => dialog.current?.showModal())} aria-haspopup="dialog" title={t("add.optionsTip")}>
              <Icon name="sliders" size={16} />
              {t("add.options")}
            </button>
          </div>
        </div>
      </form>

      {dup && <DuplicatePrompt d={dup.d} onClose={() => setDup(null)} onAction={onDuplicate} />}

      <dialog ref={dialog} className="card m-auto max-h-[92vh] w-[min(34rem,94vw)] p-0" aria-labelledby="add-title" onClose={stopCheck}>
        <form onSubmit={detailed} className="space-y-4 p-5">
          <h2 id="add-title" className="text-base font-semibold">
            {t("addd.title")}
          </h2>
          <label className="block space-y-1.5">
            <span className="label">{t("addd.url")}</span>
            <input
              className="input"
              value={url}
              onChange={(e) => {
                setUrl(e.target.value);
                if (check.s !== "idle") stopCheck();
              }}
              placeholder="https://…"
              required
            />
          </label>
          <div className="flex flex-wrap items-center gap-2">
            <button type="button" className="btn h-8 px-3 text-xs" data-testid="add-check" disabled={!url.trim() || check.s === "checking"} onClick={() => void runCheck()} title={t("intel.checkTip")}>
              <Icon name="shield" size={14} />
              {t("intel.check")}
            </button>
            {check.s === "checking" && (
              <span className="text-xs text-dim" role="status" data-testid="add-check-busy">
                {t("intel.checking")}
              </span>
            )}
            {check.s === "failed" && (
              <span className="text-xs text-dim" role="status" data-testid="add-check-failed">
                {t("intel.failed")}
              </span>
            )}
          </div>
          {check.s === "done" && <IntelligencePanel a={check.a} basics />}
          <div className="space-y-1.5" data-testid="add-folder">
            <span className="label">{t("addd.saveIn")}</span>
            <div className="flex items-center gap-2">
              <span className="selectable min-w-0 flex-1 break-all rounded-lg border border-line px-2.5 py-1.5 text-xs" data-testid="add-folder-path">
                {folder ? folder.path : remembered?.available && !opts.subdir.trim() ? remembered.path : t("addd.usualFolder")}
              </span>
              <button type="button" className="btn shrink-0" data-testid="add-change-folder" onClick={() => void chooseFolder()}>
                {t("store.change")}
              </button>
              {folder && (
                <button type="button" className="btn shrink-0" data-testid="add-default-folder" onClick={usualFolder}>
                  {t("addd.usual")}
                </button>
              )}
            </div>
            {folder ? (
              <RememberFolder testid="add-remember" kind={kind} onKind={setRememberKind} checked={remember} onChange={toggleRemember} />
            ) : (
              !opts.subdir.trim() && <RememberedNote storage={storage} kind={checkedKind} testid="add-remembered" />
            )}
          </div>
          <div className="grid grid-cols-2 gap-3">
            <label className="space-y-1.5">
              <span className="label">{t("addd.subdir")}</span>
              <input
                className="input"
                value={opts.subdir}
                disabled={!!folder}
                onChange={(e) => setOpts({ ...opts, subdir: e.target.value })}
                placeholder={folder ? t("addd.subdirChosen") : t("addd.subdirEg")}
              />
            </label>
            <label className="space-y-1.5">
              <span className="label">{t("addd.filename")}</span>
              <input className="input" value={opts.filename} onChange={(e) => setOpts({ ...opts, filename: e.target.value })} placeholder={t("addd.auto")} />
            </label>
            <label className="space-y-1.5">
              <span className="label">{t("addd.priority")}</span>
              <select className="input" value={opts.priority} onChange={(e) => setOpts({ ...opts, priority: e.target.value as Priority })}>
                {PRIORITIES.map((p) => (
                  <option key={p} value={p}>
                    {t(`prio.${p}` as Key)}
                  </option>
                ))}
              </select>
            </label>
            <label className="space-y-1.5">
              <span className="label">{t("addd.limit")}</span>
              <input className="input" value={opts.limit} onChange={(e) => setOpts({ ...opts, limit: e.target.value })} placeholder={t("addd.limitEg")} />
            </label>
          </div>
          <label className="block space-y-1.5">
            <span className="label">{t("addd.checksum")}</span>
            <input className="input font-mono text-xs" value={opts.checksum} onChange={(e) => setOpts({ ...opts, checksum: e.target.value })} placeholder="sha256:…" spellCheck={false} />
            <span className="text-xs text-dim">{t("addd.checksumHow")}</span>
          </label>

          <details className="group rounded-xl border border-line" open={advancedUsed || undefined} data-testid="add-advanced">
            <summary className="flex cursor-pointer select-none items-center gap-2 px-3 py-2 text-[13px] font-medium">
              <Icon name="chevron" size={14} className="transition-transform group-open:rotate-90" />
              {t("addd.adv")}
              <span className="font-normal text-dim">{t("addd.advFor")}</span>
            </summary>
            <div className="grid grid-cols-2 gap-3 border-t border-line p-3">
              <label className="space-y-1.5">
                <span className="label">{t("addd.user")}</span>
                <input className="input" data-testid="add-username" value={adv.username} onChange={(e) => setAdv({ ...adv, username: e.target.value })} autoComplete="off" spellCheck={false} />
              </label>
              <label className="space-y-1.5">
                <span className="label">{t("net.pass")}</span>
                <input className="input" data-testid="add-password" type="password" value={adv.password} onChange={(e) => setAdv({ ...adv, password: e.target.value })} autoComplete="off" />
              </label>
              <label className="col-span-2 space-y-1.5">
                <span className="label">{t("addd.referrer")}</span>
                <input className="input" data-testid="add-referrer" value={adv.referrer} onChange={(e) => setAdv({ ...adv, referrer: e.target.value })} placeholder="https://…" spellCheck={false} />
              </label>
              <label className="col-span-2 space-y-1.5">
                <span className="label">{t("addd.cookies")}</span>
                <input className="input font-mono text-xs" data-testid="add-cookies" value={adv.cookies} onChange={(e) => setAdv({ ...adv, cookies: e.target.value })} placeholder={t("addd.cookiesEg")} spellCheck={false} />
              </label>
              <label className="col-span-2 space-y-1.5">
                <span className="label">{t("net.ua")}</span>
                <input className="input font-mono text-xs" data-testid="add-useragent" value={adv.userAgent} onChange={(e) => setAdv({ ...adv, userAgent: e.target.value })} placeholder={t("addd.uaEg")} spellCheck={false} />
              </label>
              <p className="col-span-2 text-xs text-dim">{t("addd.sealed")}</p>
            </div>
          </details>

          <div className="grid grid-cols-2 gap-3">
            {queues.length > 1 && (
              <label className="space-y-1.5">
                <span className="label">{t("addd.queue")}</span>
                <select className="input" data-testid="add-queue" value={queueId} onChange={(e) => setQueueId(Number(e.target.value))}>
                  {queues.map((q) => (
                    <option key={q.id} value={q.id}>
                      {queueName(q)}
                    </option>
                  ))}
                </select>
              </label>
            )}
            <label className="space-y-1.5">
              <span className="label">{t("addd.when")}</span>
              <select
                className="input"
                data-testid="add-when"
                value={when}
                onChange={(e) => {
                  const v = e.target.value as typeof when;
                  setWhen(v);
                  if (v === "at" && !startAt) setStartAt(localStamp(new Date(Date.now() + 3600_000)));
                }}
              >
                <option value="now">{t("addd.startNow")}</option>
                <option value="later">{t("addd.whenLater")}</option>
                <option value="at">{t("addd.whenAt")}</option>
              </select>
            </label>
            {when === "at" && (
              <label className="space-y-1.5">
                <span className="label">{t("sq.atLabel")}</span>
                <input type="datetime-local" className="input" data-testid="add-start-at" value={startAt} onChange={(e) => setStartAt(e.target.value.slice(0, 16))} />
              </label>
            )}
          </div>
          {formError && (
            <p role="alert" className="text-xs text-bad">
              {formError}
            </p>
          )}
          <div className="flex justify-end gap-2 pt-1">
            <button type="button" className="btn" onClick={() => dialog.current?.close()}>
              {t("common.cancel")}
            </button>
            <button type="submit" className="btn btn-primary" disabled={!url.trim()}>
              {t("addd.title")}
            </button>
          </div>
        </form>
      </dialog>
    </>
  );
}
