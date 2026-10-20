// End-to-end check that the real app speaks the user's language everywhere (Phase 21): in French and in Arabic,
// every page, every Settings section and the Add and Batch dialogs are read, and English words are looked for. Only
// what the user is meant to read counts: links, file names, the product name and the technical log lines are left out.
//
//   node tests/e2e/desktop-i18n.mjs [path-to-debug-velox-desktop]

import { check, createHarness, exe, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const srv = await startFileServer(2 * 1024 * 1024);
const h = createHarness({ port: 9374, appPath: APP });
const { page, invoke } = h;
const ev = async (e) => (await page(e)).ok;
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);
const click = async (sel) => {
  await waitFor(() => has(sel), `element ${sel}`);
  return page(`(() => { document.querySelector(${JSON.stringify(sel)}).click(); return true; })()`);
};

// Words that only English sentences contain (none is a French or Arabic word, a unit or a name: French "file",
// "files", "pause" and "minute" are left out on purpose).
const ENGLISH = /\b(the|and|your|with|when|this|that|will|never|download(s|ed|ing)?|settings|speed|connections?|queue|folders?|paused|failed|completed|waiting|cancel|retry|open|show|add|remove|delete|start|stop|resume|history|update|check|browser|network|schedule|limit|today|yesterday|seconds?|hours?|left)\b/gi;

/** Visible text of the page, without links, file names, logs and the product's name. */
const visibleText = () =>
  ev(`(() => {
    const skip = (el) => el.closest('[data-testid=log-lines], [data-testid=shortcuts] kbd, .font-mono, [data-testid^=download-] [data-testid=card-name], input, textarea, code');
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

  for (const lang of ["fr", "ar"]) {
    await click("[data-testid=nav-settings]");
    await click(`[data-testid=settings-lang-${lang}]`);
    await waitFor(async () => (await ev("document.documentElement.lang")) === lang, `the ${lang} interface`);
    const found = new Map();
    const look = async (where) => {
      await sleep(400);
      const text = await visibleText();
      for (const w of english(text)) {
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
    await click("[data-testid=nav-downloads]");
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
  }
  await click("[data-testid=nav-settings]");
  await click("[data-testid=settings-lang-en]");

  // Narrow windows, every language: no page is wider than the window (below 768 px the Settings tabs used to grow to
  // the width of all of them together and push Settings sideways), the tabs scroll inside their own row, and nothing
  // is pushed out of the status bar.
  for (const lang of ["en", "fr", "ar"]) {
    await click("[data-testid=nav-settings]");
    await click(`[data-testid=settings-lang-${lang}]`);
    await waitFor(async () => (await ev("document.documentElement.lang")) === lang, `the ${lang} interface`);
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
  const langs = await ev(`[...document.querySelectorAll('[data-testid^=settings-lang-]')].map((b) => b.getAttribute('data-testid').slice('settings-lang-'.length))`);
  const navBad = [];
  for (const lang of langs) {
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

  await click("[data-testid=nav-settings]");
  await click("[data-testid=settings-lang-en]");
} catch (e) {
  check(`unexpected error: ${e.message}`, false);
} finally {
  await h.cleanup();
  srv.close();
  finish();
}
