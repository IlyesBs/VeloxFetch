// End-to-end check that the real app speaks the user's language everywhere (Phase 21): in every language it ships,
// every page, every Settings section and the Add and Batch dialogs are read, and English words are looked for. Only
// what the user is meant to read counts: links, file names, network names (the system's), the product name and the
// technical log lines are left out.
//
//   node tests/e2e/desktop-i18n.mjs [path-to-debug-velox-desktop]

import { check, createHarness, exe, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const SHOTS = path.resolve("docs/screenshots");
const srv = await startFileServer(2 * 1024 * 1024);
const h = createHarness({ port: 9374, appPath: APP });
const { page, invoke } = h;
const ev = async (e) => (await page(e)).ok;
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
/** Choose a language in Settings (a drop-down: set its value as a user would, then wait for the window to follow). */
const pickLang = async (lang) => {
  await waitFor(() => has("[data-testid=settings-lang]"), "language drop-down");
  await page(`(() => { const s = document.querySelector('[data-testid=settings-lang]');
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(s, ${JSON.stringify(lang)});
    s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
  await waitFor(async () => (await ev("document.documentElement.lang")) === lang, `the ${lang} interface`);
};
const click = async (sel) => {
  await waitFor(() => has(sel), `element ${sel}`);
  return page(`(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`);
};

// Words that only English sentences contain (none is a French or Arabic word, a unit or a name: French "file",
// "files", "pause" and "minute" are left out on purpose). Words a language shares with English are allowed in it.
const ENGLISH = /\b(the|and|your|with|when|this|that|will|never|download(s|ed|ing)?|settings|speed|connections?|queue|folders?|paused|failed|completed|waiting|cancel|retry|open|show|add|remove|delete|start|stop|resume|history|update|check|browser|network|schedule|limit|today|yesterday|seconds?|hours?|left)\b/gi;

/** Visible text of the page, without links, file names, logs and the product's name. */
const visibleText = () =>
  ev(`(() => {
    const skip = (el) => el.closest('[data-testid=log-lines], [data-testid=shortcuts] kbd, .font-mono, [data-testid^=download-] [data-testid=card-name], [data-testid=multinet-name], input, textarea, code');
    const out = [];
    const walk = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    while (walk.nextNode()) {
      const n = walk.currentNode, el = n.parentElement;
      if (!el || skip(el)) continue;
      const st = getComputedStyle(el);
      if (st.display === 'none' || st.visibility === 'hidden') continue;
      out.push(n.textContent);
    }
    for (const el of document.querySelectorAll('[aria-label], [title], [placeholder]')) {
      for (const a of ['aria-label', 'title', 'placeholder']) if (el.getAttribute(a)) out.push(el.getAttribute(a));
    }
    return out.join(' \\n ');
  })()`);

const english = (text) =>
  [
    ...new Set(
      text
        .replace(/https?:\/\/\S+/g, " ")
        .replace(/\S*[\\/]\S*/g, " ") // folder paths (a folder may be named "downloads")
        .replace(/\S+\.(bin|iso|zip|mkv|mp4|txt|json|exe|dmg|deb|rpm|AppImage)\b/gi, " ")
        .replace(/VeloxFetch/g, " ")
        .match(ENGLISH) ?? [],
    ),
  ];

try {
  await h.launch();
  await waitFor(async () => (await ev("document.querySelector('header')?.innerText ?? ''")).includes("VeloxFetch"), "UI to render");
  // A few downloads, so cards, the detail page and the history have content.
  for (const n of ["one.bin", "two.iso"]) await invoke("create_download", { request: { url: `${srv.base}/files/${n}` } });
  const failed = (await invoke("create_download", { request: { url: `${srv.base}/404/missing.zip` } })).ok;
  await sleep(2500);

  const SHARED = {
    de: ["download", "downloads", "start", "update", "browser", "limit"],
    "pt-br": ["download", "downloads", "remove"],
  };
  const langs = await ev("[...document.querySelectorAll('[data-testid=settings-lang] option, [data-testid=lang-switch] [role=radio]')].map((o) => o.value || o.dataset.testid.slice(5))");
  const all = [...new Set(langs)].filter((l) => l !== "en");
  check("every shipped language can be chosen", ["fr", "ar", "es", "pt-br", "de", "ru", "tr"].every((l) => all.includes(l)), all.join(","));
  for (const lang of all) {
    await click("[data-testid=nav-settings]");
    await pickLang(lang);
    const found = new Map();
    const overflow = [];
    const look = async (where) => {
      await sleep(400);
      // Longer words (German, Russian...) must never push the window sideways.
      const wide = await ev(`(() => { const d = document.documentElement, m = document.querySelector('main');
        const scrolls = (el) => !!el && el.scrollWidth > el.clientWidth + 1 && ['auto', 'scroll'].includes(getComputedStyle(el).overflowX);
        return { page: d.scrollWidth > d.clientWidth + 1 && getComputedStyle(document.body).overflowX !== 'hidden', main: scrolls(m), sizes: [d.scrollWidth, d.clientWidth, m?.scrollWidth, m?.clientWidth, m && getComputedStyle(m).overflowX, getComputedStyle(document.body).overflowX] }; })()`);
      if (wide?.page || wide?.main) overflow.push(`${where} ${JSON.stringify(wide.sizes)}`);
      const text = await visibleText();
      for (const w of english(text).filter((w) => !(SHARED[lang] ?? []).includes(w.toLowerCase()))) {
        found.set(w.toLowerCase(), [...(found.get(w.toLowerCase()) ?? []), where]);
        if (process.env.I18N_CONTEXT) {
          const i = text.search(new RegExp(`\\b${w}\\b`, "i"));
          console.log(`  [${where}] ${w}: …${text.slice(Math.max(0, i - 80), i + 60).replace(/\s+/g, " ")}…`);
        }
      }
    };
    for (const p of ["downloads", "active", "queue", "completed", "history"]) {
      await click(`[data-testid=nav-${p}]`);
      await look(p);
    }
    // The sidebar's Smart throttle title and mode buttons are narrow: their words must fit, never end in "…".
    await click("[data-testid=nav-downloads]");
    await sleep(300);
    const cut = await ev(`[...document.querySelectorAll('[data-testid=throttle-hud] .truncate')].filter((e) => e.offsetParent && e.scrollWidth > e.clientWidth + 1).map((e) => e.textContent)`);
    check(`${lang}: the Smart throttle title and modes fit in the sidebar`, cut.length === 0, cut.join(" | "));
    // A failed download's details page (its error, the Doctor, the diagnostics).
    await click("[data-testid=nav-downloads]");
    await click(`[data-testid=download-${failed.id}]`);
    await waitFor(() => has("[data-testid=diagnostics-card]"), "the details page");
    await look("details of a failed download");
    await click("[data-testid=nav-settings]");
    // Every section, scrolled into view one after the other.
    const sections = await ev("[...document.querySelectorAll('[data-section]')].map((s) => s.id)");
    for (const id of sections) {
      await page(`(() => { document.getElementById(${JSON.stringify(id)}).scrollIntoView(); return true; })()`);
      await look(`settings ${id}`);
    }
    // The Add dialog (Ctrl+N) and the Batch dialog (Ctrl+Shift+N).
    await page(`(() => { document.dispatchEvent(new KeyboardEvent('keydown', { key: 'n', ctrlKey: true, bubbles: true })); return true; })()`);
    await look("add dialog");
    await page(`(() => { document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); return true; })()`);
    await page(`(() => { document.dispatchEvent(new KeyboardEvent('keydown', { key: 'N', ctrlKey: true, shiftKey: true, bubbles: true })); return true; })()`);
    await look("batch dialog");
    await page(`(() => { document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); return true; })()`);
    const report = [...found].map(([w, where]) => `${w} (${[...new Set(where)].join(", ")})`);
    check(`${lang}: no English words on any page, Settings section or dialog`, report.length === 0, report.join("; "));
    check(`${lang}: nothing is wider than the window`, overflow.length === 0, overflow.join(", "));
    if (["de", "ru"].includes(lang)) {
      for (let i = 0; i < 3 && (await has("dialog[open]")); i++) {
        await page(`(() => { document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); document.querySelector('dialog[open]')?.close(); return true; })()`);
        await sleep(200);
      }
      await click("[data-testid=nav-downloads]");
      await sleep(400);
      await h.screenshot(path.join(SHOTS, `lang-${lang}-dark.png`), { width: 1280, height: 800 });
    }
  }
  await click("[data-testid=nav-settings]");
  await pickLang("en");
  check("the tray and notifications follow the chosen language", (await invoke("get_desktop_options")).ok?.language === "en");

  // First start on a computer set to French: the window opens in French, until the user picks another. (The Linux
  // web view reads the system locale, which must be installed: fr_FR is, on the test machines.)
  const es = createHarness({ port: 9375, appPath: APP, env: { LANG: "fr_FR.UTF-8", LC_ALL: "fr_FR.UTF-8", LANGUAGE: "fr_FR", VELOX_E2E_LANG: "fr-FR" } });
  try {
    await es.launch();
    const esEv = async (e) => (await es.page(e)).ok;
    await waitFor(async () => (await esEv("document.querySelector('header')?.innerText ?? ''")).includes("VeloxFetch"), "UI to render (French computer)");
    check("on a French computer the first start is in French", (await esEv("document.documentElement.lang")) === "fr" && (await esEv("document.body.innerText")).includes("Téléchargements"), await esEv("document.documentElement.lang"));
    await waitFor(async () => (await es.invoke("get_desktop_options")).ok?.language === "fr", "tray language synced");
    check("... and the tray menu and notifications are in French too", true);
    check("... nothing is saved until the user chooses", (await esEv("localStorage.getItem('velox.lang')")) === null);
  } finally {
    await es.cleanup();
  }
} catch (e) {
  check(`unexpected error: ${e.message}`, false);
} finally {
  await h.cleanup();
  srv.close();
  finish();
}
