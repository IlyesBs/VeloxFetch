import { useState } from "react";
import { api } from "../api";
import { formatSpeed } from "../format";
import { act, store, useStore } from "../hooks";
import { useT } from "../i18n";
import { copyName, effectiveLines, profileHint, profileLines, profileName, type Line } from "../profiles";
import type { ProfileAction, ProfileInfo, ProfileSettings } from "../types";
import { Icon, type IconName } from "../ui/icons";
import { Panel } from "../ui/primitives";
import { throttleStore } from "../throttle";

const ICON: Record<string, IconName> = { default: "sliders", fast: "rocket", balanced: "scale", lowResource: "leaf" };

/** Run a profile change; the answer (and the event) updates every view. */
async function run(action: ProfileAction, done?: string) {
  const p = await act(() => api.profileAction(action), done);
  if (p) {
    store.dispatch({ type: "profiles", profiles: p });
    void throttleStore.refresh(); // Smart throttle's automatic mode may have changed
  }
  return p;
}

type Tri = "keep" | "on" | "off";

/** Edit a profile of one's own (or make a new one): each setting either keeps the user's own or sets a value. */
function ProfileEditor({ initial, name: initialName, onDone, onCancel }: { initial: ProfileSettings; name: string; onDone: (name: string, s: ProfileSettings) => void; onCancel: () => void }) {
  const t = useT();
  const [name, setName] = useState(initialName);
  const [speed, setSpeed] = useState<"keep" | "none" | "set">(initial.speedLimitBps == null ? "keep" : initial.speedLimitBps === 0 ? "none" : "set");
  const [mbps, setMbps] = useState(initial.speedLimitBps ? String(Math.round((initial.speedLimitBps / 1024 ** 2) * 100) / 100) : "");
  const [active, setActive] = useState(initial.maxActive != null ? String(initial.maxActive) : "");
  const [conns, setConns] = useState(initial.maxConnections != null ? String(initial.maxConnections) : "");
  const [adaptive, setAdaptive] = useState<Tri>(initial.adaptive == null ? "keep" : initial.adaptive ? "on" : "off");
  const num = (v: string) => (v.trim() === "" ? null : Number(v));
  const a = num(active);
  const c = num(conns);
  const m = Number(mbps);
  const bad =
    name.trim() === "" ||
    (a !== null && !(Number.isInteger(a) && a >= 1 && a <= 32)) ||
    (c !== null && !(Number.isInteger(c) && c >= 1 && c <= 64)) ||
    (speed === "set" && !(m > 0));
  const settings = (): ProfileSettings => ({
    speedLimitBps: speed === "keep" ? null : speed === "none" ? 0 : Math.round(m * 1024 ** 2),
    maxActive: a,
    maxConnections: c,
    adaptive: adaptive === "keep" ? null : adaptive === "on",
  });
  return (
    <div className="mt-3 space-y-3 rounded-xl border border-line bg-raised p-3" data-testid="profile-editor">
      <label className="block space-y-1">
        <span className="label">{t("prof.name")}</span>
        <input className="input" maxLength={40} value={name} onChange={(e) => setName(e.target.value)} data-testid="profile-name" />
      </label>
      <p className="text-xs text-dim">{t("prof.keepHint")}</p>
      <div className="grid gap-3 sm:grid-cols-2">
        <label className="block space-y-1">
          <span className="label">{t("prof.f.speed")}</span>
          <div className="flex gap-2">
            <select className="input" value={speed} onChange={(e) => setSpeed(e.target.value as typeof speed)} data-testid="profile-speed">
              <option value="keep">{t("prof.keep")}</option>
              <option value="none">{t("prof.f.noLimit")}</option>
              <option value="set">{t("prof.f.limit")}</option>
            </select>
            {speed === "set" && <input className="input w-24" inputMode="decimal" aria-label={t("prof.f.mbps")} placeholder="MB/s" value={mbps} onChange={(e) => setMbps(e.target.value)} />}
          </div>
        </label>
        <label className="block space-y-1">
          <span className="label">{t("prof.f.active")}</span>
          <input className="input" inputMode="numeric" placeholder={t("prof.keep")} value={active} onChange={(e) => setActive(e.target.value)} data-testid="profile-active" />
        </label>
        <label className="block space-y-1">
          <span className="label">{t("prof.f.connections")}</span>
          <input className="input" inputMode="numeric" placeholder={t("prof.keep")} value={conns} onChange={(e) => setConns(e.target.value)} data-testid="profile-connections" />
        </label>
        <label className="block space-y-1">
          <span className="label">{t("prof.f.adaptive")}</span>
          <select className="input" value={adaptive} onChange={(e) => setAdaptive(e.target.value as Tri)} data-testid="profile-adaptive">
            <option value="keep">{t("prof.keep")}</option>
            <option value="on">{t("prof.f.on")}</option>
            <option value="off">{t("prof.f.off")}</option>
          </select>
        </label>
      </div>
      <div className="flex justify-end gap-2">
        <button className="btn" onClick={onCancel}>
          {t("common.cancel")}
        </button>
        <button className="btn btn-primary" disabled={bad} onClick={() => onDone(name.trim(), settings())} data-testid="profile-save">
          {t("common.save")}
        </button>
      </div>
    </div>
  );
}

/**
 * Download Profiles: presets over the settings below (speed, downloads at once, connections, Smart throttle's
 * automatic mode). Choosing one applies it at once; your own settings are kept for "Default".
 */
export function ProfilesSettings() {
  const t = useT();
  const say = (l: Line) => t(l.key, l.vars);
  const p = useStore((s) => s.profiles);
  const [editing, setEditing] = useState<string | "new" | null>(null);
  // A delete asks once more on the same row (a second press deletes).
  const [confirming, setConfirming] = useState<string | null>(null);
  if (!p) return null;
  const customNames = p.profiles.filter((x) => !x.builtIn).map((x) => x.name ?? "");

  const select = (x: ProfileInfo) => x.id !== p.active && void run({ kind: "select", id: x.id }, t("prof.selected", { name: profileName(x, t) }));
  const duplicate = (x: ProfileInfo) => void run({ kind: "duplicate", id: x.id, name: copyName(profileName(x, t), customNames, t) }, t("prof.duplicated"));
  const remove = (x: ProfileInfo) => {
    if (confirming !== x.id) return setConfirming(x.id);
    setConfirming(null);
    void run({ kind: "delete", id: x.id }, t("prof.deleted"));
  };

  return (
    <Panel title={t("prof.title")} hint={t("prof.hint")}>
      <div role="radiogroup" aria-label={t("prof.title")} className="space-y-2" data-testid="profiles">
        {p.profiles.map((x) => {
          const on = x.id === p.active;
          const hint = profileHint(x);
          return (
            <div key={x.id} className={`rounded-xl border px-3 py-2.5 ${on ? "border-accent bg-accent-soft" : "border-line"}`} data-testid={`profile-${x.id}`} data-active={on}>
              <div className="flex items-start gap-3">
                <button role="radio" aria-checked={on} className="flex min-w-0 flex-1 items-start gap-3 text-start" onClick={() => select(x)}>
                  <Icon name={ICON[x.id] ?? "gear"} size={18} className={`mt-0.5 shrink-0 ${on ? "text-accent" : "text-dim"}`} />
                  <span className="min-w-0">
                    <span className="block text-[13px] font-semibold">
                      {profileName(x, t)}
                      {on && <span className="chip ms-2 bg-accent text-white">{t("prof.active")}</span>}
                    </span>
                    {hint && <span className="block text-xs text-dim">{t(hint)}</span>}
                    <span className="mt-1 flex flex-wrap gap-1">
                      {profileLines(x.settings).map((l, i) => (
                        <span key={i} className="chip bg-raised text-dim">
                          {say(l)}
                        </span>
                      ))}
                    </span>
                  </span>
                </button>
                <div className="flex shrink-0 gap-1">
                  <button className="btn btn-ghost btn-icon h-8" title={t("prof.duplicate")} aria-label={t("prof.duplicate")} onClick={() => duplicate(x)} data-testid={`profile-dup-${x.id}`}>
                    <Icon name="copy" size={14} />
                  </button>
                  {!x.builtIn && (
                    <>
                      <button className="btn btn-ghost btn-icon h-8" title={t("prof.edit")} aria-label={t("prof.edit")} onClick={() => setEditing(x.id)} data-testid={`profile-edit-${x.id}`}>
                        <Icon name="sliders" size={14} />
                      </button>
                      {confirming === x.id ? (
                        <button className="btn h-8 text-bad" onClick={() => remove(x)} onBlur={() => setConfirming(null)} autoFocus data-testid={`profile-del-${x.id}`}>
                          {t("prof.deleteConfirm")}
                        </button>
                      ) : (
                        <button className="btn btn-ghost btn-icon h-8" title={t("prof.delete")} aria-label={t("prof.delete")} onClick={() => remove(x)} data-testid={`profile-del-${x.id}`}>
                          <Icon name="trash" size={14} />
                        </button>
                      )}
                    </>
                  )}
                </div>
              </div>
              {editing === x.id && (
                <ProfileEditor
                  initial={x.settings}
                  name={x.name ?? ""}
                  onCancel={() => setEditing(null)}
                  onDone={(name, settings) => void run({ kind: "update", id: x.id, name, settings }, t("prof.saved")).then((r) => r && setEditing(null))}
                />
              )}
            </div>
          );
        })}
      </div>
      {editing === "new" ? (
        <ProfileEditor
          initial={{}}
          name=""
          onCancel={() => setEditing(null)}
          onDone={(name, settings) => void run({ kind: "create", name, settings }, t("prof.saved")).then((r) => r && setEditing(null))}
        />
      ) : (
        <button className="btn mt-3" onClick={() => setEditing("new")} data-testid="profile-new">
          <Icon name="plus" size={14} /> {t("prof.new")}
        </button>
      )}
      <div className="mt-4 border-t border-line pt-3 text-xs" data-testid="profile-effective">
        <p className="font-medium">{t("prof.inForce")}</p>
        <p className="mt-1 text-dim">{effectiveLines(p.effective).map(say).join(" · ")}</p>
        <p className="mt-2 text-dim">{t("prof.live")}</p>
        {p.effective.speedLimitBps !== p.base.speedLimitBps && (
          <p className="mt-1 text-dim">{t("prof.yourLimit", { speed: p.base.speedLimitBps === null ? t("common.none") : formatSpeed(p.base.speedLimitBps) })}</p>
        )}
      </div>
    </Panel>
  );
}

/** The compact switch for the status bar: the active profile, and the list to change it. */
export function ProfileSwitch() {
  const t = useT();
  const p = useStore((s) => s.profiles);
  if (!p) return null;
  const label = (x: ProfileInfo) => profileName(x, t);
  return (
    <label className="flex items-center gap-1 text-fg/85" title={t("prof.switchTip")}>
      <Icon name="sliders" size={13} className="text-accent" />
      <select
        className="cursor-pointer bg-transparent text-[12px] outline-none"
        aria-label={t("prof.title")}
        value={p.active}
        data-testid="profile-switch"
        onChange={(e) => {
          const x = p.profiles.find((y) => y.id === e.target.value);
          if (x) void run({ kind: "select", id: x.id }, t("prof.selected", { name: label(x) }));
        }}
      >
        {p.profiles.map((x) => (
          <option key={x.id} value={x.id}>
            {label(x)}
          </option>
        ))}
      </select>
    </label>
  );
}

