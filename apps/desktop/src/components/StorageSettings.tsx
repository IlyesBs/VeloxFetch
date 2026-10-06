import { useCallback, useEffect, useState } from "react";
import { api, normalizeError } from "../api";
import { act, store } from "../hooks";
import { useT, type Key } from "../i18n";
import { Rich } from "../ui/rich";
import type { FileKind, Storage } from "../types";
import { Icon } from "../ui/icons";
import { Panel } from "../ui/primitives";

const KIND_LABEL: Record<FileKind, Key> = {
  video: "fold.video",
  audio: "fold.audio",
  image: "fold.image",
  document: "fold.document",
  archive: "fold.archive",
  program: "fold.program",
  other: "fold.other",
};

/**
 * Where downloads go: the folder (chosen in the system's own folder dialog, never typed here) and whether
 * new files are sorted into a folder per type. Existing files are never moved.
 */
export function StorageSettings() {
  const t = useT();
  const [state, setState] = useState<Storage | null>(null);
  const [names, setNames] = useState<Record<string, string>>({});

  const adopt = useCallback((s: Storage) => {
    setState(s);
    setNames(Object.fromEntries(s.folders.map((f) => [f.kind, f.folder])));
  }, []);

  const refresh = useCallback(async () => {
    try {
      adopt(await api.getStorage());
    } catch (e) {
      store.toast("error", normalizeError(e).message);
    }
  }, [adopt]);
  useEffect(() => void refresh(), [refresh]);

  const change = async () => {
    const next = await act(() => api.chooseDownloadFolder());
    if (next) {
      adopt(next);
      store.toast("info", t("store.chosen"));
    }
  };
  const reset = async () => {
    const next = await act(() => api.resetDownloadFolder(), t("store.reset"));
    if (next) adopt(next);
  };
  const toggle = async (on: boolean) => {
    const next = await act(() => api.setOrganize(on), on ? t("store.sortOn") : t("store.sortOff"));
    if (next) adopt(next);
  };
  const dirty = !!state && state.folders.some((f) => (names[f.kind] ?? f.folder).trim() !== f.folder);
  const saveNames = async () => {
    if (!state) return;
    // Send only what differs from the defaults; an empty box means "the default name".
    const changed = Object.fromEntries(state.folders.map((f) => [f.kind, (names[f.kind] ?? f.folder).trim() === f.defaultFolder ? "" : (names[f.kind] ?? f.folder).trim()]));
    const next = await act(() => api.setFolderNames(changed), t("store.namesSaved"));
    if (next) adopt(next);
  };

  return (
    <Panel title={t("store.title")} hint={t("store.hint")}>
      <p className="text-xs text-dim">{t("store.saveIn")}</p>
      <div className="mt-1 flex items-center gap-2">
        <span className="selectable min-w-0 flex-1 break-all rounded-lg border border-line px-2.5 py-1.5 text-[13px]" data-testid="storage-root">
          {state?.root ?? "…"}
        </span>
        <button className="btn shrink-0" data-testid="storage-change" disabled={!state} onClick={() => void change()}>
          {t("store.change")}
        </button>
        <button className="btn shrink-0" data-testid="storage-reset" disabled={!state || state.isDefault} onClick={() => void reset()}>
          {t("store.usual")}
        </button>
        <button className="btn btn-ghost btn-icon shrink-0" aria-label={t("store.open")} title={t("store.open")} data-testid="storage-open" onClick={() => void act(() => api.openDownloadFolder())}>
          <Icon name="folder" size={16} />
        </button>
      </div>
      <p className="mt-2 text-xs text-dim">
        {t("store.how")}
      </p>

      <label className="mt-4 flex cursor-pointer items-center gap-2 text-[13px]">
        <input type="checkbox" data-testid="organize-toggle" checked={state?.organize ?? false} disabled={!state} onChange={(e) => void toggle(e.target.checked)} />
        {t("store.sort")}
      </label>
      <p className="mt-1 text-xs text-dim">
        <Rich text={t("store.sortHow", { audio: names.audio ?? "Audio", doc: names.document ?? "Documents", prog: names.program ?? "Programs" })} />
      </p>

      {state?.organize && (
        <div className="mt-3" data-testid="organize-names">
          <p className="text-xs font-medium text-dim">{t("store.names")}</p>
          <div className="mt-1 grid gap-2 sm:grid-cols-2">
            {state.folders.map((f) => (
              <label key={f.kind} className="flex items-center gap-2 text-xs">
                <span className="w-44 shrink-0 text-dim">{t(KIND_LABEL[f.kind])}</span>
                <input
                  className="input h-8 flex-1"
                  data-testid={`folder-name-${f.kind}`}
                  aria-label={t("store.folderFor", { kind: t(KIND_LABEL[f.kind]) })}
                  value={names[f.kind] ?? f.folder}
                  maxLength={60}
                  spellCheck={false}
                  onChange={(e) => setNames({ ...names, [f.kind]: e.target.value })}
                />
              </label>
            ))}
          </div>
          <button className="btn btn-primary mt-3" data-testid="folder-names-save" disabled={!dirty} onClick={() => void saveNames()}>
            {t("store.saveNames")}
          </button>
        </div>
      )}
    </Panel>
  );
}
