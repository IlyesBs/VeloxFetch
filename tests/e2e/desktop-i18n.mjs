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
        .replace(/VeloxFetch|Firefox Add-ons|Chrome Web Store/g, " ") // names, never translated
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
    de: ["download", "downloads", "start", "update", "browser", "limit", "add"], // German writes "Add-on"
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
    // Network usage (issue #45): "Today" / "This month" and the longest numbers fit the card at every window size.
    const tight = [];
    for (const [width, height] of [[1360, 860], [1100, 760], [900, 640], [760, 560], [640, 480]]) {
      await h.cdp.send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 1, mobile: false });
      await sleep(250);
      const bad = await ev(`(() => { const rows = [...document.querySelectorAll('[data-testid=network-period] > div')];
        if (rows.length !== 2) return ['rows: ' + rows.length];
        for (const dd of document.querySelectorAll('[data-testid=network-period] dd')) dd.textContent = '1023.9 MB';
        const out = [];
        for (const r of rows) { const dt = r.querySelector('dt'), dd = r.querySelector('dd');
          if (dt.scrollWidth > dt.clientWidth + 1 || r.scrollWidth > r.clientWidth + 1 || dd.getBoundingClientRect().right > r.getBoundingClientRect().right + 1 || dd.getBoundingClientRect().left < r.getBoundingClientRect().left - 1) out.push(dt.textContent + ' ' + r.clientWidth); }
        return out; })()`);
      if (bad.length) tight.push(`${width}: ${bad.join(", ")}`);
    }
    await h.cdp.send("Emulation.clearDeviceMetricsOverride");
    check(`${lang}: the Network usage card's today and month fit at every window size`, tight.length === 0, tight.join(" | "));
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

  // Narrow windows, every language: no page is wider than the window (below 768 px the Settings tabs used to grow to
  // the width of all of them together and push Settings sideways), the tabs scroll inside their own row, and nothing
  // is pushed out of the status bar.
  for (const lang of ["en", "fr", "ar", "es", "pt-br", "de", "ru", "tr"]) {
    await click("[data-testid=nav-settings]");
    await pickLang(lang);
    const wide = [];
    for (const [width, height] of [[900, 640], [760, 560], [640, 480]]) {
      await h.cdp.send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 1, mobile: false });
      for (const p of ["downloads", "queue", "history", "settings"]) {
        await click(`[data-testid=nav-${p}]`);
        await sleep(300);
        const bad = await ev(`(() => { const out = []; const m = document.querySelector('[data-testid=main]');
          // Elements against the page's box (not its scroll width: right to left, the Linux web view counts its
          // scrollbar once more there, a known quirk the page's padding already covers). Tabs scroll in their row.
          const box = m.getBoundingClientRect(), row = document.querySelector('[data-testid=settings-nav] > div');
          // Inside a row made to scroll sideways (tab bars) or a clipping box, parts out of view are by design: that
          // row itself is checked.
          const clipped = (e) => { for (let p = e.parentElement; p && p !== m; p = p.parentElement) if (getComputedStyle(p).overflowX !== 'visible') return true; return false; };
          const sticking = [...m.querySelectorAll('*')].filter((e) => e.offsetParent && !(row && row.contains(e)) && !clipped(e) && getComputedStyle(e).position !== 'fixed')
            .filter((e) => { const r = e.getBoundingClientRect(); return r.width > 0 && (r.right > box.right + 1 || r.left < box.left - 1); });
          const outer = sticking.filter((e) => !sticking.some((o) => o !== e && o.contains(e)));
          for (const e of outer.slice(0, 3)) out.push('sticks out: ' + (e.getAttribute('data-testid') || e.tagName + '.' + String(e.className).slice(0, 40)) + ' ' + Math.round(e.getBoundingClientRect().left) + '-' + Math.round(e.getBoundingClientRect().right) + ' of ' + Math.round(box.left) + '-' + Math.round(box.right));
          const nav = document.querySelector('[data-testid=settings-nav]');
          if (nav) { const r = nav.getBoundingClientRect(); if (r.right > window.innerWidth + 1 || r.left < -1) out.push('tabs row ' + Math.round(r.left) + '-' + Math.round(r.right));
            for (const b of nav.querySelectorAll('button')) if (b.getBoundingClientRect().width > 320) { out.push('tab ' + Math.round(b.getBoundingClientRect().width) + ' px'); break; } }
          for (const e of document.querySelector('[data-testid=statusbar]').children) { const r = e.getBoundingClientRect();
            if (e.offsetParent && (r.right > window.innerWidth + 1 || r.left < -1)) out.push('status bar: ' + (e.innerText || e.className)); }
          return out; })()`);
        if (bad.length) wide.push(`${width} ${p}: ${bad.join(", ")}`);
      }
    }
    await h.cdp.send("Emulation.clearDeviceMetricsOverride");
    check(`${lang}: no page is wider than a narrow window, the Settings tabs scroll in their row, the status bar fits`, wide.length === 0, wide.join(" | "));
  }

  // The Settings section tabs sit in a narrow vertical column (>= 768 px). A long label — French "File d'attente et
  // planification" / "Navigateur et presse-papiers", and the longer German / Portuguese ones — must wrap and grow
  // its tab, never overflow the tab's height and overlap the next one. Checked in every language the app offers.
  await h.cdp.send("Emulation.setDeviceMetricsOverride", { width: 1100, height: 900, deviceScaleFactor: 1, mobile: false });
  await click("[data-testid=nav-settings]");
  const navLangs = await ev(`[...document.querySelectorAll('[data-testid^=settings-lang-]')].map((b) => b.getAttribute('data-testid').slice('settings-lang-'.length))`);
  const navBad = [];
  for (const lang of navLangs) {
    await click(`[data-testid=settings-lang-${lang}]`);
    await waitFor(async () => (await ev("document.documentElement.lang")) === lang, `the ${lang} interface`);
    await sleep(150);
    const bad = await ev(`(() => { const out = []; const btns = [...document.querySelectorAll('[data-testid=settings-nav] button')];
      for (const b of btns) if (b.scrollHeight > b.clientHeight + 1) out.push((b.innerText || '').replace(/\\s+/g, ' ').trim() + ' ' + b.scrollHeight + '>' + b.clientHeight);
      for (let i = 1; i < btns.length; i++) { const a = btns[i - 1].getBoundingClientRect(), c = btns[i].getBoundingClientRect(); if (c.top < a.bottom - 1) out.push('tabs overlap at ' + i); }
      return out; })()`);
    if (bad.length) navBad.push(`${lang}: ${bad.join(", ")}`);
  }
  await h.cdp.send("Emulation.clearDeviceMetricsOverride");
  check("every Settings section tab holds its label without overflow or overlap in every language (long labels wrap)", navBad.length === 0, navBad.join(" | "));

  await pickLang("en");

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
