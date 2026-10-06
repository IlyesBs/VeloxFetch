import { useEffect, useRef, useState, type FormEvent } from "react";
import { createPortal } from "react-dom";
import { api } from "../api";
import { hostOf } from "../format";
import { act } from "../hooks";
import { useT } from "../i18n";
import type { Download } from "../types";
import { Icon } from "../ui/icons";

/**
 * The server of a download asked for a user name and password: ask for them and start it again.
 * The password is sent once to Rust, kept with the download sealed on disk, and never shown again.
 */
export function SignInDialog({ download, onClose }: { download: Download; onClose: () => void }) {
  const t = useT();
  const [user, setUser] = useState("");
  const [pass, setPass] = useState("");
  const [show, setShow] = useState(false);
  const [busy, setBusy] = useState(false);
  const first = useRef<HTMLInputElement>(null);

  useEffect(() => first.current?.focus(), []);
  useEffect(() => {
    const esc = (e: KeyboardEvent) => e.key === "Escape" && !busy && onClose();
    document.addEventListener("keydown", esc);
    return () => document.removeEventListener("keydown", esc);
  }, [busy, onClose]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!user.trim()) return;
    setBusy(true);
    const ok = await act(() => api.signIn(download.id, user.trim(), pass), t("sign.ok"));
    setBusy(false);
    if (ok) onClose();
  };

  return createPortal(
    <div
      className="fixed inset-0 z-40 grid place-items-center bg-black/45 p-4"
      // React events cross portals: keep clicks here from reaching the card that opened the dialog.
      onClick={(e) => {
        e.stopPropagation();
        if (e.target === e.currentTarget && !busy) onClose();
      }}
      onContextMenu={(e) => e.stopPropagation()}
    >
      <form className="card rise w-full max-w-sm space-y-4 p-5 shadow-xl" role="dialog" aria-modal="true" aria-labelledby="signin-title" data-testid="signin-dialog" onSubmit={submit}>
        <div className="flex items-center gap-3">
          <div className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-accent-soft text-accent">
            <Icon name="lock" size={19} />
          </div>
          <div className="min-w-0">
            <h2 id="signin-title" className="text-[15px] font-semibold">
              {t("sign.title", { host: hostOf(download.url) })}
            </h2>
            <p className="truncate text-xs text-dim">{t("sign.for", { name: download.filename })}</p>
          </div>
        </div>
        <label className="block space-y-1.5">
          <span className="label">{t("addd.user")}</span>
          <input ref={first} className="input" data-testid="signin-user" value={user} onChange={(e) => setUser(e.target.value)} autoComplete="off" spellCheck={false} />
        </label>
        <label className="block space-y-1.5">
          <span className="label">{t("net.pass")}</span>
          <div className="relative">
            <input
              className="input pe-16"
              data-testid="signin-pass"
              type={show ? "text" : "password"}
              value={pass}
              onChange={(e) => setPass(e.target.value)}
              autoComplete="off"
              spellCheck={false}
            />
            <button type="button" className="absolute end-2 top-1/2 -translate-y-1/2 text-xs text-dim hover:text-fg" onClick={() => setShow(!show)}>
              {show ? t("sign.hide") : t("sign.show")}
            </button>
          </div>
        </label>
        <p className="text-xs text-dim">{t("sign.kept")}</p>
        <div className="flex justify-end gap-2">
          <button type="button" className="btn" onClick={onClose} disabled={busy}>
            {t("common.cancel")}
          </button>
          <button type="submit" className="btn btn-primary" data-testid="signin-submit" disabled={busy || !user.trim()}>
            {t("sign.submit")}
          </button>
        </div>
      </form>
    </div>,
    document.body,
  );
}
