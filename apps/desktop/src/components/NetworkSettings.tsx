import { useEffect, useState } from "react";
import { api, normalizeError } from "../api";
import { act, store } from "../hooks";
import { useT, type Key } from "../i18n";
import type { NetworkSettings as Net, ProxyMode } from "../types";
import { Panel } from "../ui/primitives";

const MODES: { mode: ProxyMode; label: Key; hint: Key }[] = [
  { mode: "system", label: "net.system", hint: "net.systemHint" },
  { mode: "none", label: "net.none", hint: "net.noneHint" },
  { mode: "manual", label: "net.manual", hint: "net.manualHint" },
];

/** Proxy and user agent. The proxy password is sent once and never shown again. */
export function NetworkSettings({ id }: { id?: string }) {
  const t = useT();
  const [net, setNet] = useState<Net | null>(null);
  const [mode, setMode] = useState<ProxyMode>("system");
  const [url, setUrl] = useState("");
  const [user, setUser] = useState("");
  const [pass, setPass] = useState("");
  const [passTouched, setPassTouched] = useState(false);
  const [ua, setUa] = useState("");
  const [busy, setBusy] = useState(false);

  const adopt = (n: Net) => {
    setNet(n);
    setMode(n.proxyMode);
    setUrl(n.proxyUrl ?? "");
    setUser(n.proxyUsername ?? "");
    setPass("");
    setPassTouched(false);
    setUa(n.userAgent ?? "");
  };

  useEffect(() => {
    api.getNetwork().then(adopt, (e) => store.toast("error", normalizeError(e).message));
  }, []);

  const dirty =
    !!net &&
    (mode !== net.proxyMode || url.trim() !== (net.proxyUrl ?? "") || user.trim() !== (net.proxyUsername ?? "") || passTouched || ua.trim() !== (net.userAgent ?? ""));

  const save = async () => {
    setBusy(true);
    const next = await act(
      () =>
        api.setNetwork({
          proxyMode: mode,
          proxyUrl: url.trim(),
          proxyUsername: user.trim(),
          ...(passTouched && { proxyPassword: pass }),
          userAgent: ua.trim(),
        }),
      t("net.saved"),
    );
    setBusy(false);
    if (next) adopt(next);
  };

  return (
    <Panel id={id} title={t("net.title")} hint={t("net.hint")}>
      <div role="radiogroup" aria-label={t("net.proxy")} className="inline-flex rounded-xl border border-line bg-raised p-1">
        {MODES.map((m) => (
          <button
            key={m.mode}
            role="radio"
            aria-checked={mode === m.mode}
            data-testid={`proxy-${m.mode}`}
            title={t(m.hint)}
            onClick={() => setMode(m.mode)}
            className={`flex h-8 items-center rounded-lg px-3 text-[13px] font-medium transition-colors duration-150 ${mode === m.mode ? "bg-surface text-fg shadow-sm" : "text-dim hover:text-fg"}`}
          >
            {t(m.label)}
          </button>
        ))}
      </div>
      <p className="mt-1.5 text-xs text-dim">{t(MODES.find((m) => m.mode === mode)!.hint)}</p>

      {mode === "manual" && (
        <div className="mt-3 grid gap-3 sm:grid-cols-2" data-testid="proxy-fields">
          <label className="space-y-1.5 sm:col-span-2">
            <span className="label">{t("net.address")}</span>
            <input className="input font-mono text-xs" data-testid="proxy-url" value={url} onChange={(e) => setUrl(e.target.value)} placeholder={t("net.addressEg")} spellCheck={false} />
          </label>
          <label className="space-y-1.5">
            <span className="label">{t("net.user")}</span>
            <input className="input" data-testid="proxy-user" value={user} onChange={(e) => setUser(e.target.value)} autoComplete="off" spellCheck={false} />
          </label>
          <label className="space-y-1.5">
            <span className="label">{t("net.pass")}</span>
            <input
              className="input"
              type="password"
              data-testid="proxy-pass"
              value={pass}
              placeholder={net?.proxyHasPassword && !passTouched ? t("net.passSaved") : ""}
              onChange={(e) => {
                setPass(e.target.value);
                setPassTouched(true);
              }}
              autoComplete="off"
            />
          </label>
          {net && !net.secretsProtected && user && <p className="text-xs text-warn sm:col-span-2">{t("net.noVault")}</p>}
        </div>
      )}

      <label className="mt-4 block space-y-1.5">
        <span className="label">{t("net.ua")}</span>
        <input className="input font-mono text-xs" data-testid="user-agent" value={ua} onChange={(e) => setUa(e.target.value)} placeholder={net?.defaultUserAgent ?? ""} spellCheck={false} />
        <span className="text-xs text-dim">{t("net.uaHint")}</span>
      </label>

      <div className="mt-4 flex gap-2">
        <button className="btn btn-primary" data-testid="network-save" disabled={!dirty || busy} onClick={() => void save()}>
          {t("common.save")}
        </button>
        {dirty && (
          <button className="btn" onClick={() => net && adopt(net)} disabled={busy}>
            {t("common.undo")}
          </button>
        )}
      </div>
    </Panel>
  );
}
