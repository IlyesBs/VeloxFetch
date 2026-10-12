// End-to-end test of issue #20 "cards are flashing when I launch a download", in the real desktop app.
//  * Starting, resuming and pausing downloads moves cards: up the dashboard's list, between the Active page's
//    sections. A browser restarts the CSS animations of an element put back into the page, so every moved card
//    replayed its entrance (fade in from nothing): the list blinked. Each card now fades in once, when it is new.
//  * The connection grid's cells pulse at a speed that follows the download's. Changing it changed the animations'
//    duration, so every lit cell jumped to another point of its beat at once (the grid blinked while a download's
//    speed settled); the pace now changes smoothly.
//
//   node tests/e2e/desktop-card-flash.mjs [path-to-debug-velox-desktop.exe]

import { check, createHarness, exe, finish, path, sleep, startFileServer, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const srv = await startFileServer(48 * 1024 * 1024);
const h = createHarness({ port: 9371, appPath: APP });
const { page, invoke } = h;
const ev = (e) => page(e).then((r) => r.ok);
const status = async (id) => (await invoke("get_download", { id })).ok?.status;

// What the page saw: entrances, cards put back into the page, the lowest opacity of each card already there,
// and restarts of grid cells that stayed lit.
const RECORDER = `(() => {
  const r = (window.__flash = { rise: [], moved: 0, minOpacity: {}, restarts: 0, cellStarts: 0, on: true });
  const started = new WeakSet();
  const cards = () => document.querySelectorAll('[data-testid^="download-"]');
  const before = new Set([...cards()].map((c) => c.dataset.testid));
  document.addEventListener('animationstart', (e) => {
    if (!r.on) return;
    if (e.animationName === 'rise') r.rise.push(e.target.dataset?.testid ?? '?');
    if (e.animationName === 'flicker') {
      r.cellStarts++;
      if (started.has(e.target)) r.restarts++;
      started.add(e.target);
    }
  }, true);
  new MutationObserver((ms) => {
    for (const m of ms) {
      if (m.type === 'attributes' && m.target.localName === 'i' && !m.target.classList.contains('on')) started.delete(m.target);
      for (const n of m.addedNodes) if (n.nodeType === 1 && n.matches('[data-testid^="download-"]') && before.has(n.dataset.testid)) r.moved++;
    }
  }).observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ['class'] });
  // Each lit cell's place in its pulse (iterations done + progress): it must move on smoothly from frame to frame.
  // Changing the pulse's duration made every cell jump to another place at once (a blink).
  const pos = new WeakMap();
  r.jumps = 0;
  r.steps = 0;
  const frame = (now) => {
    if (!r.on) return;
    for (const a of document.querySelector('.matrix')?.getAnimations({ subtree: true }) ?? []) {
      if (a.animationName !== 'flicker') continue; // not the cells' 300 ms colour transition
      const t = a.effect?.getComputedTiming();
      if (!t || t.progress === null || !(t.duration > 0)) continue;
      const p = t.currentIteration + t.progress;
      const prev = pos.get(a);
      if (prev) {
        const allowed = ((now - prev.at) * a.playbackRate) / t.duration * 1.5 + 0.05;
        r.steps++;
        if (p - prev.p > allowed || p < prev.p - 0.01) {
          r.jumps++;
          if ((r.why ??= []).length < 12) r.why.push({ p: +p.toFixed(3), prev: +prev.p.toFixed(3), dt: Math.round(now - prev.at), rate: +a.playbackRate.toFixed(3), prate: +prev.rate.toFixed(3), d: t.duration, pd: prev.d, ct: Math.round(a.currentTime), pct: Math.round(prev.ct), card: a.effect.target.closest('[data-testid^="download-"]')?.dataset.testid });
        }
      }
      pos.set(a, { p, at: now, rate: a.playbackRate, d: t.duration, ct: a.currentTime });
    }
    for (const c of cards()) if (before.has(c.dataset.testid)) {
      const o = Number(getComputedStyle(c).opacity);
      r.minOpacity[c.dataset.testid] = Math.min(r.minOpacity[c.dataset.testid] ?? 1, o);
    }
    requestAnimationFrame(frame);
  };
  requestAnimationFrame(frame);
  return [...before];
})()`;
const stop = () => ev(`(window.__flash.on = false, JSON.stringify(window.__flash))`).then(JSON.parse);

/** Speed swings, so the grid's pulse changes several times while the downloads run. */
async function swing(ms) {
  const end = Date.now() + ms;
  for (let i = 0; Date.now() < end; i++) {
    srv.rateBps = [256, 4096, 768, 2048][i % 4] * 1024;
    await sleep(600);
  }
}

try {
  await h.launch();
  await waitFor(async () => (await ev("document.querySelector('header')?.innerText ?? ''")).includes("VeloxFetch"), "UI to render");
  await ev(`(() => { window.__errs = []; window.addEventListener('error', (e) => window.__errs.push(e.message)); return true; })()`);

  // A realistic list: finished files, then two paused downloads.
  for (const n of ["one.zip", "two.pdf", "three.iso"]) {
    const d = (await invoke("create_download", { request: { url: `${srv.base}/${n}` } })).ok;
    await waitFor(async () => (await status(d.id)) === "completed", `${n} to finish`, 60000);
  }
  srv.rateBps = 512 * 1024;
  const paused = [];
  for (const n of ["four.bin", "five.bin"]) {
    const d = (await invoke("create_download", { request: { url: `${srv.base}/${n}` } })).ok;
    await waitFor(async () => (await status(d.id)) === "downloading", `${n} to start`);
    await invoke("pause_download", { id: d.id });
    await waitFor(async () => (await status(d.id)) === "paused", `${n} to pause`);
    paused.push(d.id);
  }

  // ---- Downloads (dashboard): a paused download resumed, and a new one added from the bar
  await ev(`(document.querySelector('[data-testid=nav-downloads]').click(), true)`);
  await waitFor(() => ev(`document.querySelectorAll('[data-testid^="download-"]').length === 5`), "the five cards");
  await sleep(800); // the cards' own entrance is over
  const existing = await ev(RECORDER);
  await invoke("resume_download", { id: paused[1] });
  await ev(`(() => { const el = document.querySelector('[data-testid=url-input]');
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(el, ${JSON.stringify(`${srv.base}/six.bin`)});
    el.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
  await sleep(100);
  await ev(`(document.querySelector('[data-testid=add-button]').click(), true)`);
  await swing(4000);
  let r = await stop();
  const newCards = r.rise.filter((id) => !existing.includes(id));
  check("the scenario moves cards (a resumed download goes up the list)", r.moved > 0, JSON.stringify(r));
  check("no card already there plays its entrance again (no flash)", r.rise.every((id) => !existing.includes(id)), JSON.stringify(r.rise));
  check("every card already there stays fully visible all along", Object.values(r.minOpacity).every((o) => o > 0.99), JSON.stringify(r.minOpacity));
  check("the new download's card fades in once", newCards.length === 1, JSON.stringify(r.rise));
  check("the grid's lit cells never restart while the speed changes", r.cellStarts > 0 && r.restarts === 0, JSON.stringify({ starts: r.cellStarts, restarts: r.restarts }));
  check("the grid's pulse changes pace smoothly (no cell jumps in its beat)", r.steps > 100 && r.jumps === 0, JSON.stringify({ steps: r.steps, jumps: r.jumps, why: r.why }));

  // ---- Active: cards change section (running -> paused -> running)
  await ev(`(document.querySelector('[data-testid=nav-active]').click(), true)`);
  await waitFor(() => ev(`!!document.querySelector('[data-testid=active-view]')`), "the Active page");
  await sleep(800);
  const onActive = await ev(RECORDER);
  await invoke("resume_download", { id: paused[0] });
  await sleep(1200);
  await invoke("pause_download", { id: paused[1] });
  await swing(3000);
  r = await stop();
  check("Active: a card changing section does not fade in again", r.rise.length === 0, JSON.stringify(r.rise));
  check("Active: every card stays fully visible", Object.values(r.minOpacity).every((o) => o > 0.99), JSON.stringify({ onActive, min: r.minOpacity }));
  check("Active: the grid's lit cells never restart or jump", r.restarts === 0 && r.jumps === 0, JSON.stringify({ starts: r.cellStarts, restarts: r.restarts, jumps: r.jumps, why: r.why }));

  // ---- A pulse still follows the speed (the grid is not frozen)
  const rates = await ev(`JSON.stringify(document.querySelector('.matrix')?.getAnimations({ subtree: true }).filter((a) => a.animationName === 'flicker').map((a) => a.playbackRate) ?? [])`);
  check("the lit cells pulse at the download's pace", JSON.parse(rates).length > 0 && JSON.parse(rates).every((x) => x > 0), rates);
  check("no page errors", (await ev(`JSON.stringify(window.__errs ?? [])`)) === "[]");
} catch (e) {
  check("the run completed", false, String(e?.stack ?? e));
} finally {
  srv.close();
  await h.cleanup();
}
finish();
