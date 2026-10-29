import { useT, type Key } from "../i18n";
import type { FileKind, Storage } from "../types";

/** What each kind of file is called in the folder settings and in "Remember this folder for ...". */
export const KIND_LABEL: Record<FileKind, Key> = {
  video: "fold.video",
  audio: "fold.audio",
  image: "fold.image",
  document: "fold.document",
  archive: "fold.archive",
  program: "fold.program",
  other: "fold.other",
};

const KINDS = Object.keys(KIND_LABEL) as FileKind[];

/** The folder remembered for `kind`, and whether it can be used now (`null` = none remembered or kind unknown). */
export function rememberedFor(storage: Storage | null, kind: FileKind | null): { path: string; available: boolean } | null {
  const f = kind ? storage?.folders.find((x) => x.kind === kind) : undefined;
  return f?.remembered ? { path: f.remembered, available: f.rememberedAvailable } : null;
}

/**
 * "Remember this folder for <kind>", next to a folder picked for one download. With `onKind`, the kind can be
 * chosen (the Add dialog may not know it yet); otherwise it is the one the app found.
 */
export function RememberFolder(props: {
  kind: FileKind | null;
  onKind?: (kind: FileKind) => void;
  checked: boolean;
  onChange: (on: boolean) => void;
  disabled?: boolean;
  testid: string;
}) {
  const t = useT();
  const { kind, onKind, checked, onChange, disabled, testid } = props;
  return (
    <div className="space-y-0.5" data-testid={testid}>
      <div className="flex flex-wrap items-center gap-2 text-[13px]">
        <label className="flex cursor-pointer items-center gap-2">
          <input type="checkbox" data-testid={`${testid}-box`} checked={checked} disabled={disabled} onChange={(e) => onChange(e.target.checked)} />
          {onKind ? t("folder.rememberFor") : t("folder.remember", { kind: kind ? t(KIND_LABEL[kind]) : "" })}
        </label>
        {onKind && (
          <select
            className="input h-7 w-auto py-0 text-xs"
            data-testid={`${testid}-kind`}
            aria-label={t("folder.rememberFor")}
            value={kind ?? ""}
            disabled={disabled}
            onChange={(e) => onKind(e.target.value as FileKind)}
          >
            {!kind && <option value="">{t("folder.chooseKind")}</option>}
            {KINDS.map((k) => (
              <option key={k} value={k}>
                {t(KIND_LABEL[k])}
              </option>
            ))}
          </select>
        )}
      </div>
      {checked && <p className="ms-6 text-xs text-dim">{t("folder.rememberHint")}</p>}
    </div>
  );
}

/** "remembered for <kind>" (or "not available") under the path the dialog shows when nothing was picked. */
export function RememberedNote({ storage, kind, testid }: { storage: Storage | null; kind: FileKind | null; testid: string }) {
  const t = useT();
  const r = rememberedFor(storage, kind);
  if (!r || !kind) return null;
  return (
    <p className="text-xs text-dim" data-testid={testid}>
      {r.available ? t("folder.remembered", { kind: t(KIND_LABEL[kind]) }) : t("folder.rememberedGone", { kind: t(KIND_LABEL[kind]) })}
    </p>
  );
}
