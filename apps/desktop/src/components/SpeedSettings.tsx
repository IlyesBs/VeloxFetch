import { useCallback, useEffect, useState } from "react";
import { actions } from "../actions";
import { api, normalizeError } from "../api";
import { formatSpeed, parseRate } from "../format";
import { act, store, useStore } from "../hooks";
import { t as tr, useT, type Key } from "../i18n";
import type { FileKind, SpeedLimits } from "../types";
import { Icon } from "../ui/icons";
import { Panel } from "../ui/primitives";
import { FileIcon, KINDS } from "./FileIcon";
import { activeProfile, overridden, profileName } from "../profiles";

const KIND_PLURAL: Record<FileKind, Key> = {
  video: "kinds.video",
  audio: "kinds.audio",
  image: "kinds.image",
  document: "kinds.document",
  archive: "kinds.archive",
  program: "kinds.program",
  other: "kinds.other",
};
const kindName = (k: FileKind) => tr(KIND_PLURAL[k]);

/** One kind of file: its limit, and a box to change it. */
function TypeLimit({ kind, limit, onChanged }: { kind: FileKind; limit: number | undefined; onChanged: () => void }) {
  const t = useT();
  const [value, setValue] = useState("");
  const apply = async (bps: number | null) => {
    const name = kindName(kind);
    const ok = await act(
      () => api.setSpeedLimit({ kind: "category", name: kind }, bps),
      bps ? t("speed.typeLimited", { kind: name, speed: formatSpeed(bps) }) : t("speed.typeFree", { kind: name }),
    );
    if (ok !== undefined) {
      setValue("");
      onChanged();
    }
  };
  const parsed = parseRate(value);
  return (
    <div className="flex items-center gap-2.5 py-1.5" data-testid={`type-limit-${kind}`}>
      <FileIcon kind={kind} size={28} />
      <span className="w-32 shrink-0 text-[13px]">{kindName(kind)}</span>
      <span className={`num w-24 shrink-0 text-xs ${limit ? "font-medium text-fg" : "text-dim"}`}>{limit ? formatSpeed(limit) : t("speed.noLimit")}</span>
      <input
        className="input h-8 max-w-28"
        aria-label={t("speed.limitFor", { kind: kindName(kind) })}
        placeholder={t("speed.eg2M")}
        value={value}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => e.key === "Enter" && parsed && void apply(parsed)}
      />
      <button className="btn h-8" disabled={!parsed} onClick={() => parsed && void apply(parsed)}>
        {t("common.set")}
      </button>
      {limit ? (
        <button className="btn btn-ghost btn-icon h-8" aria-label={t("speed.removeFor", { kind: kindName(kind) })} title={t("speed.removeLimit")} onClick={() => void apply(null)}>
          <Icon name="x" size={14} />
        </button>
      ) : null}
    </div>
  );
}

/** The global limit, and limits per kind of file (e.g. keep videos from filling the line). */
export function SpeedSettings({ id }: { id?: string }) {
  const t = useT();
  const limit = useStore((s) => s.metrics.globalLimit);
  const profiles = useStore((s) => s.profiles);
  const over = overridden(profiles);
  const prof = activeProfile(profiles);
  const [value, setValue] = useState("");
  const [unit, setUnit] = useState<"K" | "M">("M");
  const [limits, setLimits] = useState<SpeedLimits | null>(null);
  const [showTypes, setShowTypes] = useState(false);

  const refresh = useCallback(() => {
    api.getSpeedLimits().then(
      (l) => {
        setLimits(l);
        if (Object.keys(l.byType).length > 0) setShowTypes(true);
      },
      (e) => store.toast("error", normalizeError(e).message),
    );
  }, []);
  useEffect(refresh, [refresh]);

  const apply = () => {
    const n = Number(value);
    if (!Number.isFinite(n) || n <= 0) return;
    void actions.setGlobalLimit(Math.round(n * (unit === "M" ? 1024 ** 2 : 1024)));
    setValue("");
  };

  return (
    <Panel id={id} title={t("speed.title")} hint={t("speed.hint")}>
      <p className="mb-3 text-[13px]">
        {t("speed.current")} <strong data-testid="global-limit">{limit ? formatSpeed(limit) : t("common.none")}</strong>
      </p>
      {over.speed && prof && profiles && (
        <p className="-mt-2 mb-3 text-[11px] text-accent" data-testid="profile-note">
          {t("prof.noteLimit", {
            name: profileName(prof, t),
            yours: profiles.base.speedLimitBps === null ? t("common.none") : formatSpeed(profiles.base.speedLimitBps),
          })}
        </p>
      )}
      <div className="flex max-w-md gap-2">
        <input
          className="input"
          inputMode="decimal"
          aria-label={t("speed.global")}
          placeholder={t("speed.eg5")}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && apply()}
        />
        <select className="input w-28" aria-label={t("speed.unit")} value={unit} onChange={(e) => setUnit(e.target.value as "K" | "M")}>
          <option value="K">KB/s</option>
          <option value="M">MB/s</option>
        </select>
        <button className="btn btn-primary" onClick={apply} disabled={!(Number(value) > 0)}>
          {t("common.apply")}
        </button>
        <button className="btn" onClick={() => void actions.setGlobalLimit(null)} disabled={!limit}>
          {t("common.remove")}
        </button>
      </div>

      <div className="mt-5 border-t border-line pt-4">
        <button className="flex items-center gap-2 text-[13px] font-medium" aria-expanded={showTypes} data-testid="type-limits-toggle" onClick={() => setShowTypes(!showTypes)}>
          <Icon name="chevron" size={14} className={`transition-transform ${showTypes ? "rotate-90" : ""}`} />
          {t("speed.byType")}
          {limits && Object.keys(limits.byType).length > 0 && (
            <span className="chip bg-accent-soft text-accent">{t("speed.nSet", { n: Object.keys(limits.byType).length })}</span>
          )}
        </button>
        {showTypes && (
          <div className="mt-2 divide-y divide-line" data-testid="type-limits">
            <p className="pb-2 text-xs text-dim">{t("speed.byTypeHint")}</p>
            {KINDS.map((k) => (
              <TypeLimit key={k} kind={k} limit={limits?.byType[k]} onChanged={refresh} />
            ))}
          </div>
        )}
      </div>
    </Panel>
  );
}
