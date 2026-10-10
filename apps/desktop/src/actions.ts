import { api } from "./api";
import { act, store } from "./hooks";
import { t, type Key } from "./i18n";
import type { Download, FileTarget, PlaceDownload, PowerAction, Priority, QueueAction, SaveQueue, Schedule } from "./types";

/** UI-level actions. Failures surface as toasts (see `act`). */
export const actions = {
  start: (id: number) => act(() => api.startDownload(id)),
  pause: (id: number) => act(() => api.pauseDownload(id)),
  resume: (id: number) => act(() => api.resumeDownload(id)),
  cancel: (id: number) => act(() => api.cancelDownload(id)),
  retry: (id: number) => act(() => api.retryDownload(id)),
  /** Download it again from the start (same link, folder and name; never overwrites a file that is there). */
  redownload: (d: Download) => act(() => api.redownload(d.id), t("act.redownloading", { name: d.filename })),
  remove: (id: number, deleteFile = false) => act(() => api.deleteDownload(id, deleteFile)),
  /** Remove a finished download and delete its file from the disk (ask first: see DeleteConfirm). */
  deleteFile: (d: Download) => act(() => api.deleteDownload(d.id, true), t("done.deleted", { name: d.filename })),
  setPriority: (id: number, p: Priority) => act(() => api.setPriority(id, p)),
  setDownloadLimit: (id: number, bps: number | null) =>
    act(() => api.setSpeedLimit({ kind: "download", id }, bps), bps ? t("act.limitOn") : t("act.limitOff")),
  setGlobalLimit: (bps: number | null) =>
    act(() => api.setSpeedLimit({ kind: "global" }, bps), bps ? t("act.globalOn") : t("act.globalOff")),
  pauseQueue: (pauseRunning: boolean) =>
    act(() => api.pauseQueue(pauseRunning), pauseRunning ? t("act.qPaused") : t("act.qPausedFinish")),
  resumeQueue: () => act(() => api.resumeQueue(), t("act.qResumed")),
  setMaxActive: (n: number) => act(() => api.setQueueConfig({ maxActive: n })),
  setMaxConnections: (n: number) => act(() => api.setQueueConfig({ maxConnections: n })),
  setSchedule: (s: Schedule) => act(() => api.setSchedule(s), s.enabled ? t("act.schedSaved") : t("act.schedOff")),
  setKeepAwake: (on: boolean) => act(() => api.setKeepAwake(on), on ? t("act.keepAwakeOn") : t("act.keepAwakeOff")),
  /** Smart Download Queue: the answer also arrives as an event; it is applied at once for a snappy page. */
  saveQueue: async (req: SaveQueue, done: string) => {
    const q = await act(() => api.saveQueue(req), done);
    if (q) store.dispatch({ type: "queues", queues: q });
    return q;
  },
  deleteQueue: async (id: number) => {
    const q = await act(() => api.deleteQueue(id), t("act.sqDeleted"));
    if (q) store.dispatch({ type: "queues", queues: q });
    return q;
  },
  queueAction: async (id: number, action: QueueAction, done?: string) => {
    const said = done ?? (action.kind === "start" ? t("act.sqStarted") : action.kind === "pause" ? t("act.sqPaused") : undefined);
    const q = await act(() => api.queueAction(id, action), said);
    if (q) store.dispatch({ type: "queues", queues: q });
    return q;
  },
  /** Move a download to a queue, change when it starts, or move it up / down. */
  place: (req: PlaceDownload, done?: string) => act(() => api.placeDownload(req), done),
  setPowerAction: (a: PowerAction | null) =>
    act(() => api.setPowerAction(a), a ? t("act.power", { action: t(`power.${a}` as Key).toLowerCase() }) : t("act.powerOff")),
  showInFolder: (target: FileTarget) => act(() => api.showInFolder(target)),
  openFile: (target: FileTarget) => act(() => api.openFile(target)),
  /** Add a link again (from the history), as a new download. */
  downloadAgain: (url: string) => act(() => api.createDownload({ url }), t("act.again")),
  /** Run `fn` on every download in `ids`, one after the other, then say how it went in one toast. */
  bulk: async (ids: number[], fn: (id: number) => Promise<unknown>, done: (n: number) => string) => {
    let ok = 0;
    for (const id of ids) {
      try {
        await fn(id);
        ok++;
      } catch {
        /* one failure must not stop the others; counted below */
      }
    }
    store.toast(ok === ids.length ? "info" : "error", done(ok) + (ok < ids.length ? ` ${t("act.notChanged", { n: ids.length - ok })}` : ""));
  },
  resumeAll: (list: Download[]) =>
    actions.bulk(
      list.map((d) => d.id),
      (id) => api.resumeDownload(id),
      (n) => t("act.resumed", { n }),
    ),
  retryAll: (list: Download[]) =>
    actions.bulk(
      list.map((d) => d.id),
      (id) => api.retryDownload(id),
      (n) => t("act.retrying", { n }),
    ),
  pauseAll: (list: Download[]) =>
    actions.bulk(
      list.map((d) => d.id),
      (id) => api.pauseDownload(id),
      (n) => t("act.paused", { n }),
    ),
  clearCompleted: (list: Download[]) =>
    actions.bulk(
      list.map((d) => d.id),
      (id) => api.deleteDownload(id, false),
      (n) => t("act.cleared", { n }),
    ),
  copy: async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      return false;
    }
  },
};
