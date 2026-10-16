import { useEffect, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { createSpyLock } from "../spyLock";
import { LANGS, setLang, useLang, useT, type Key } from "../i18n";
import { getThemePref, setThemePref, subscribeThemePref, type ThemePref } from "../theme";
import { Icon, type IconName } from "../ui/icons";
import { Panel } from "../ui/primitives";
import { BrowserSettings } from "../components/BrowserSettings";
import { ClipboardSettings } from "../components/ClipboardSettings";
import { DesktopSettings } from "../components/DesktopSettings";
import { DiagnosticsSettings } from "../components/DiagnosticsSettings";
import { MultiNetworkSettings } from "../components/MultiNetworkSettings";
import { NetworkSettings } from "../components/NetworkSettings";
import { PowerSettings } from "../components/PowerSettings";
import { ProfilesSettings } from "../components/ProfilesSettings";
import { ScheduleSettings } from "../components/ScheduleSettings";
import { SmartThrottleSettings } from "../components/SmartThrottleSettings";
import { SpeedSettings } from "../components/SpeedSettings";
import { StorageSettings } from "../components/StorageSettings";
import { UpdateSettings } from "../components/UpdateSettings";
import { VideoToolsSettings } from "../components/VideoToolsSettings";
import { IntegritySettings } from "../components/IntegritySettings";

const THEMES: { pref: ThemePref; label: Key; icon: IconName }[] = [
  { pref: "system", label: "theme.system", icon: "gauge" },
  { pref: "light", label: "theme.light", icon: "sun" },
  { pref: "dark", label: "theme.dark", icon: "moon" },
];

/** The sections of the settings page, in order (the side menu jumps to them). */
const SECTIONS: { id: string; label: Key; icon: IconName }[] = [
  { id: "set-general", label: "sec.appearance", icon: "palette" },
  { id: "set-folders", label: "sec.folders", icon: "folder" },
  { id: "set-speed", label: "sec.speed", icon: "gauge" },
  { id: "set-network", label: "sec.network", icon: "globe" },
  { id: "set-schedule", label: "sec.schedule", icon: "calendar" },
  { id: "set-browser", label: "sec.browser", icon: "browser" },
  { id: "set-keys", label: "sec.keys", icon: "keyboard" },
  { id: "set-diagnostics", label: "sec.diagnostics", icon: "activity" },
  { id: "set-updates", label: "sec.updates", icon: "download" },
  { id: "set-about", label: "sec.about", icon: "info" },
];

export const SHORTCUTS: { keys: string[]; what: Key }[] = [
  { keys: ["Ctrl", "V"], what: "keys.paste" },
  { keys: ["Ctrl", "L"], what: "keys.linkBar" },
  { keys: ["Ctrl", "N"], what: "keys.add" },
  { keys: ["Ctrl", "Shift", "N"], what: "keys.addMany" },
  { keys: ["Ctrl", "F"], what: "keys.search" },
  { keys: ["Ctrl", "1"], what: "nav.downloads" },
  { keys: ["Ctrl", "2"], what: "nav.queue" },
  { keys: ["Ctrl", "3"], what: "nav.history" },
  { keys: ["Ctrl", ","], what: "nav.settings" },
  { keys: ["Esc"], what: "keys.esc" },
];

function Group({ id, children }: { id: string; children: ReactNode }) {
  return (
    <div id={id} className="scroll-mt-4 space-y-4" data-section>
      {children}
    </div>
  );
}

export function SettingsView() {
  const t = useT();
  const lang = useLang();
  // in step with the title bar's theme button
  const theme = useSyncExternalStore(subscribeThemePref, getThemePref);
  const [current, setCurrent] = useState(SECTIONS[0]!.id);
  const content = useRef<HTMLDivElement>(null);
  const lock = useRef(createSpyLock());

  // Highlight the section being read: the last one whose top has scrolled past the top of the view
  // (the page scrolls inside <main>; at the very bottom, the last section).
  useEffect(() => {
    const root = document.querySelector<HTMLElement>("[data-testid=main]");
    if (!root) return;
    const onScroll = () => {
      if (lock.current.scrolled()) return; // scrolling to the section the user chose
      const top = root.getBoundingClientRect().top + 96;
      const sections = [...(content.current?.querySelectorAll<HTMLElement>("[data-section]") ?? [])];
      const atEnd = root.scrollTop + root.clientHeight >= root.scrollHeight - 4;
      const passed = sections.filter((el) => el.getBoundingClientRect().top <= top);
      const pick = atEnd ? sections.at(-1) : (passed.at(-1) ?? sections[0]);
      if (pick) setCurrent(pick.id);
    };
    onScroll();
    const byHand = () => lock.current.release();
    root.addEventListener("scroll", onScroll, { passive: true });
    for (const e of ["wheel", "touchstart", "keydown"] as const) root.addEventListener(e, byHand, { passive: true });
    return () => {
      root.removeEventListener("scroll", onScroll);
      for (const e of ["wheel", "touchstart", "keydown"] as const) root.removeEventListener(e, byHand);
    };
  }, []);

  const jump = (id: string) => {
    setCurrent(id);
    lock.current.pin(id);
    document.getElementById(id)?.scrollIntoView({ behavior: "smooth", block: "start" });
  };

  return (
    <div className="grid gap-6 md:grid-cols-[11rem_1fr]">
      <nav aria-label={t("sec.nav")} className="md:sticky md:top-0 md:self-start" data-testid="settings-nav">
        <div className="flex gap-1 overflow-x-auto pb-1 md:flex-col md:overflow-visible md:pb-0">
          {SECTIONS.map((s) => (
            <button key={s.id} className="nav-link shrink-0 md:shrink" aria-current={current === s.id} onClick={() => jump(s.id)}>
              <Icon name={s.icon} size={15} />
              {t(s.label)}
            </button>
          ))}
        </div>
      </nav>

      <div ref={content} className="min-w-0 space-y-4">
        <Group id="set-general">
          <Panel title={t("set.appearance")} hint={t("set.appearanceHint")}>
            <div role="radiogroup" aria-label={t("set.theme")} className="inline-flex rounded-xl border border-line bg-raised p-1">
              {THEMES.map((th) => (
                <button
                  key={th.pref}
                  role="radio"
                  aria-checked={theme === th.pref}
                  data-testid={`theme-${th.pref}`}
                  onClick={() => setThemePref(th.pref)}
                  className={`flex h-8 items-center gap-1.5 rounded-lg px-3 text-[13px] font-medium transition-colors duration-150 ${
                    theme === th.pref ? "bg-solid text-fg shadow-sm" : "text-dim hover:text-fg"
                  }`}
                >
                  <Icon name={th.icon} size={14} />
                  {t(th.label)}
                </button>
              ))}
            </div>
          </Panel>
          <Panel title={t("set.language")} hint={t("set.languageHint")}>
            {/* Each language named in itself, so anyone finds theirs whatever the current one. */}
            <select className="input w-full max-w-xs" aria-label={t("set.language")} value={lang} data-testid="settings-lang" onChange={(e) => setLang(e.target.value)}>
              {LANGS.map((l) => (
                <option key={l.lang} value={l.lang} lang={l.lang}>
                  {l.label}
                </option>
              ))}
            </select>
          </Panel>
          <DesktopSettings />
        </Group>

        <Group id="set-folders">
          <StorageSettings />
          <IntegritySettings />
        </Group>

        <Group id="set-speed">
          <ProfilesSettings />
          <SpeedSettings />
          <SmartThrottleSettings />
        </Group>

        <Group id="set-network">
          <NetworkSettings />
          <MultiNetworkSettings />
        </Group>

        <Group id="set-schedule">
          <ScheduleSettings />
          <PowerSettings />
        </Group>

        <Group id="set-browser">
          <BrowserSettings />
          <VideoToolsSettings />
          <ClipboardSettings />
        </Group>

        <Group id="set-keys">
          <Panel title={t("keys.title")} hint={t("keys.hint")}>
            <dl className="divide-y divide-line" data-testid="shortcuts">
              {SHORTCUTS.map((s) => (
                <div key={s.what} className="flex items-center gap-3 py-2">
                  <dt className="flex w-36 shrink-0 gap-1">
                    {s.keys.map((k) => (
                      <kbd key={k} className="kbd">
                        {k}
                      </kbd>
                    ))}
                  </dt>
                  <dd className="text-[13px] text-dim">{t(s.what)}</dd>
                </div>
              ))}
            </dl>
            <p className="mt-3 text-xs text-dim">{t("keys.drop")}</p>
          </Panel>
        </Group>

        <Group id="set-diagnostics">
          <DiagnosticsSettings />
        </Group>

        <Group id="set-updates">
          <UpdateSettings />
        </Group>

        <Group id="set-about">
          <Panel title={t("about.title")} hint={t("about.hint")}>
            <p className="text-xs text-dim">{t("about.body")}</p>
            <p className="num mt-2 text-xs text-dim" data-testid="about-version">
              {t("about.version", { v: __APP_VERSION__ })}
            </p>
          </Panel>
        </Group>
      </div>
    </div>
  );
}
