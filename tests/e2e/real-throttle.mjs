// By hand, on a computer with the internet: Smart throttle's three levels in the real desktop app on the real line
// (no stand-ins). A big download runs at full speed; a real 4K YouTube video is fetched next to it (yt-dlp, as a
// player buffering ahead), and a listed program (curl) downloads next to it.
//
//   Fast:   the video starts next to the download: nothing slows the download.
//   Smart:  the video is found within a minute (the download takes almost the whole line, so the video only shows
//           as a little traffic, or when downloads make room for a moment), downloads give way so the video gets the
//           line, and they take it back when it stops. Nobody else: never slowed for nobody. A listed program
//           slows them while it runs.
//   Manual: Aggressive < Balanced < Performance, each applied at once and kept, whatever runs.
//
//   node tests/e2e/real-throttle.mjs [path-to-debug-velox-desktop]
// Needs curl and yt-dlp on PATH. VELOX_LINE_MBPS: the line's speed (MB/s, default 3).

import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import { check, createHarness, exe, finish, path, sleep } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const BIG = "https://mirrors.edge.kernel.org/debian-cd/current/amd64/iso-dvd/debian-13.7.0-amd64-DVD-1.iso";
const VIDEO = "https://www.youtube.com/watch?v=LXb3EKWsInQ"; // a 4K nature film
const LISTED = "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.10.tar.xz";
const h = createHarness({ port: 9383, appPath: APP });
const { invoke } = h;
const MB = 1024 * 1024;
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "velox-real-throttle-"));
let other = null;
let otherDir = null;
let dl = null;

const fmt = (b) => `${(b / MB).toFixed(2)} MB/s`;
const smart = async () => (await invoke("get_smart_throttle")).ok;
const set = async (request) => {
  const r = await invoke("set_smart_throttle", { request });
  if (r.err) throw new Error(`set_smart_throttle: ${JSON.stringify(r.err)}`);
  return r.ok;
};
const mySpeed = async () => (await invoke("get_download", { id: dl.id })).ok?.speedBps ?? 0;

/** The other program: the 4K video (yt-dlp) or the listed program (curl). Its bytes land in its own folder. */
function startOther(kind) {
  otherDir = fs.mkdtempSync(path.join(tmp, `${kind}-`));
  other =
    kind === "video"
      ? spawn("yt-dlp", ["-q", "--no-warnings", "-f", "bv*[height>=2160]", "-o", path.join(otherDir, "v.%(ext)s"), VIDEO], { stdio: "ignore" })
      : spawn("curl", ["-s", "-o", path.join(otherDir, "f.bin"), LISTED], { stdio: "ignore" });
}
function stopOther() {
  other?.kill("SIGKILL");
  other = null;
}
const otherBytes = () => {
  try {
    return fs.readdirSync(otherDir).reduce((a, f) => a + fs.statSync(path.join(otherDir, f)).size, 0);
  } catch {
    return 0;
  }
};
async function otherSpeed(secs) {
  const a = otherBytes();
  await sleep(secs * 1000);
  return (otherBytes() - a) / secs;
}

/** Watch every second for at most `ms` until `done(state)`; logs each change of reason / cap. */
async function watch(ms, done, log) {
  const t0 = Date.now();
  let last = "";
  while (Date.now() - t0 < ms) {
    const s = await smart();
    const line = `${s.reason} ${s.limitBps ? fmt(s.limitBps) : "-"}`;
    if (line !== last) {
      log.push(`${((Date.now() - t0) / 1000).toFixed(0)}s ${line} (other ${fmt(s.otherBps ?? 0)}, downloads ${fmt(await mySpeed())})`);
      last = line;
    }
    if (done(s)) return s;
    await sleep(1000);
  }
  return null;
}

try {
  await h.launch();
  await set({ enabled: false, auto: true, apps: [] }); // Fast
  const r = await invoke("create_download", { request: { url: BIG } });
  if (r.err) throw new Error(JSON.stringify(r.err));
  dl = r.ok;
  // Until it takes the whole line (85 % of VELOX_LINE_MBPS), at most 3 minutes.
  const line = Number(process.env.VELOX_LINE_MBPS ?? 3) * MB;
  let alone = 0;
  for (let i = 0; i < 60 && alone < 0.85 * line; i++) {
    await sleep(3000);
    alone = await mySpeed();
  }
  console.log(`downloads alone: ${fmt(alone)}`);
  check("the download takes the line", alone >= 0.85 * line, fmt(alone));

  // ---- Fast: the video next to it changes nothing.
  startOther("video");
  await sleep(15000);
  const fastLog = [];
  await watch(30000, () => false, fastLog);
  const starved = await otherSpeed(6);
  console.log(`Fast: ${fastLog.join(" | ")}; the video next to the download: ${fmt(starved)}`);
  check("Fast: never slowed", fastLog.every((l) => l.includes("off -")), fastLog.join(" | "));

  // ---- Smart: the video is found, downloads give way, and take the line back.
  await set({ enabled: true, auto: true });
  const log = [];
  const found = await watch(60000, (s) => s.reason === "browsing" || s.reason === "busy", log);
  console.log(log.join("\n"));
  check("Smart finds the video (within a minute)", !!found, log.at(-1));
  await sleep(8000);
  const given = await otherSpeed(10);
  const slowed = await mySpeed();
  const s1 = await smart();
  console.log(`Smart: the video ${fmt(given)} (was ${fmt(starved)} next to full speed), downloads ${fmt(slowed)}, cap ${fmt(s1.limitBps ?? 0)}`);
  check("the video gets the line now (at least twice what it had, and 1 MB/s)", given >= Math.max(2 * starved, 1 * MB), `${fmt(starved)} -> ${fmt(given)}`);
  check("downloads keep to the cap and keep moving", slowed > 0 && slowed <= (s1.limitBps ?? 0) * 1.2, `${fmt(slowed)} vs ${fmt(s1.limitBps ?? 0)}`);
  const held = [];
  for (let i = 0; i < 20; i++) {
    held.push((await smart()).reason);
    await sleep(1500);
  }
  check("held while the video downloads (30 s)", held.every((x) => ["browsing", "busy", "easing"].includes(x)), held.join(","));
  stopOther();
  const back = [];
  const free = await watch(90000, (s) => s.limitBps === null, back);
  console.log(back.join("\n"));
  check("downloads take the whole line back when the video stops (within 90 s)", !!free, back.at(-1));
  await sleep(10000);
  const again = await mySpeed();
  check("full speed again", again > alone * 0.7, `${fmt(again)} vs ${fmt(alone)}`);
  const quiet = [];
  await watch(90000, () => false, quiet);
  console.log(quiet.join("\n"));
  check("nobody else: checks find nobody, never slowed for nobody (90 s)", !quiet.some((l) => /browsing|busy/.test(l)), quiet.join(" | "));

  // A listed program (Smart): slowed while it runs, lifted when it closes.
  await set({ apps: ["curl"], gamePolicy: "adaptive" });
  startOther("listed");
  const app = await watch(15000, (s) => s.reason === "app", []);
  check("a listed program slows downloads (within 15 s)", !!app && app.limitBps > 0, JSON.stringify(app));
  await sleep(10000);
  const capped = await mySpeed();
  const given2 = await otherSpeed(8);
  console.log(`listed program: downloads ${fmt(capped)} (cap ${fmt(app?.limitBps ?? 0)}), the program ${fmt(given2)}`);
  check("downloads keep to the program's cap", capped <= (app?.limitBps ?? 0) * 1.2, fmt(capped));
  check("the listed program gets the line", given2 >= 1 * MB, fmt(given2));
  stopOther();
  const lifted = await watch(20000, (s) => s.reason !== "app", []);
  check("lifted when the program closes (within 20 s)", !!lifted, JSON.stringify(lifted));
  await set({ apps: [] });

  // ---- Manual: each level applied at once and kept.
  await set({ auto: false, sharePercent: 25 });
  const caps = {};
  for (const share of [10, 25, 50]) {
    await set({ sharePercent: share });
    await sleep(1500);
    const s = await smart();
    caps[share] = s.limitBps;
    check(`Manual ${share}: applied at once`, s.reason === "manual" && s.limitBps > 0, JSON.stringify(s));
  }
  console.log(`Manual caps: ${JSON.stringify(Object.fromEntries(Object.entries(caps).map(([k, v]) => [k, fmt(v ?? 0)])))}`);
  check("Aggressive < Balanced < Performance", caps[10] < caps[25] && caps[25] < caps[50], JSON.stringify(caps));
  await set({ sharePercent: 10 });
  await sleep(10000);
  const aggr = await mySpeed();
  check("downloads keep to Aggressive's cap", aggr > 0 && aggr <= caps[10] * 1.2, `${fmt(aggr)} vs ${fmt(caps[10])}`);
  const kept = await watch(20000, (s) => s.reason !== "manual" || s.limitBps !== caps[10], []);
  check("Manual is kept (nothing else changes it)", kept === null);
  await set({ enabled: false });
  await sleep(1500);
  check("back to Fast: no cap at once", (await smart()).limitBps === null);
} catch (e) {
  check(`unexpected error: ${e.message}`, false);
} finally {
  stopOther();
  try {
    if (dl) await invoke("delete_download", { id: dl.id, deleteFile: true });
    await set({ enabled: false, auto: true, apps: [], sharePercent: 25 });
  } catch {}
  await h.cleanup();
  fs.rmSync(tmp, { recursive: true, force: true });
  finish();
}
