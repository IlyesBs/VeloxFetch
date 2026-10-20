// By hand, with the internet: the publisher's checksum (feature #6) with real files from real mirrors, in the real
// desktop app (https, no test switch). The Linux kernel (cdn.kernel.org, `sha256sums.asc`, a signed list) and a Debian
// installer image (mirrors.edge.kernel.org, `SHA512SUMS` / `SHA256SUMS`) are found beforehand by the Add dialog's
// check, verified before they are finished, and the files on disk really have the published digests; a file its
// folder's list does not name is finished as before, not verified.
//
//   node tests/e2e/real-publisher-checksum.mjs [path-to-debug-velox-desktop]

import fs from "node:fs";
import crypto from "node:crypto";
import { check, createHarness, exe, finish, path, sleep, waitFor } from "./harness.mjs";

const APP = path.resolve(process.argv[2] ?? exe("target/debug/velox-desktop"));
const KERNEL = "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.10.tar.xz";
const DEBIAN_DIR = "https://mirrors.edge.kernel.org/debian-cd/current/amd64/iso-cd/";
const UNLISTED = "https://cdn.kernel.org/pub/linux/kernel/v6.x/sha256sums.asc";
const h = createHarness({ port: 9393, appPath: APP });
const { page, invoke } = h;
const ev = (expr) => page(expr).then((r) => r.ok);
const has = (sel) => ev(`!!document.querySelector(${JSON.stringify(sel)})`);

const digest = (file, algo) =>
  new Promise((resolve, reject) => {
    const h = crypto.createHash(algo);
    fs.createReadStream(file).on("data", (d) => h.update(d)).on("end", () => resolve(h.digest("hex"))).on("error", reject);
  });
const listed = async (listUrl, name) => {
  const text = await (await fetch(listUrl)).text();
  return text.split("\n").map((l) => l.trim().split(/\s+\*?/)).find(([hex, n]) => n === name && /^[0-9a-f]{64,128}$/.test(hex))?.[0];
};
const run = async (url, minutes) => {
  const t = Date.now();
  const d = (await invoke("create_download", { request: { url } })).ok;
  let seenReady = false;
  while (Date.now() - t < minutes * 60000) {
    const now = (await invoke("get_download", { id: d.id })).ok;
    if (now.verify?.state === "ready" && now.status === "downloading") seenReady = true;
    if (["completed", "failed"].includes(now.status)) return { d: now, secs: (Date.now() - t) / 1000, seenReady };
    await sleep(1000);
  }
  return { d: (await invoke("get_download", { id: d.id })).ok, secs: (Date.now() - t) / 1000, seenReady };
};

try {
  await h.launch();
  await waitFor(async () => (await ev("document.querySelector('header')?.innerText ?? ''")).includes("VeloxFetch"), "UI to render");
  const sums = await (await fetch(`${DEBIAN_DIR}SHA256SUMS`)).text();
  const debName = sums.split("\n").map((l) => l.trim().split(/\s+/)[1]).find((n) => /^debian-[\d.]+-amd64-netinst\.iso$/.test(n ?? ""));
  check("the current Debian installer image is found on the mirror", !!debName, debName);
  const DEBIAN = `${DEBIAN_DIR}${debName}`;

  // ---- the Add dialog's check says it beforehand
  const ak = (await invoke("analyze_link", { request: { url: KERNEL } })).ok;
  check("kernel: the check finds the signed list sha256sums.asc", ak?.publisherChecksum?.list === "sha256sums.asc", JSON.stringify(ak?.publisherChecksum));
  const ad = (await invoke("analyze_link", { request: { url: DEBIAN } })).ok;
  check("Debian: the check finds SHA512SUMS (the strongest)", ad?.publisherChecksum?.list === "SHA512SUMS" && ad.publisherChecksum.algo === "SHA-512", JSON.stringify(ad?.publisherChecksum));
  const au = (await invoke("analyze_link", { request: { url: UNLISTED } })).ok;
  check("a file its folder's list does not name: nothing found", !au?.publisherChecksum, JSON.stringify(au?.publisherChecksum));

  // ---- the kernel: 145 MB, verified
  const k = await run(KERNEL, 15);
  console.log(`kernel: ${k.d.status} in ${k.secs.toFixed(0)} s (${(145142812 / k.secs / 1e6).toFixed(2)} MB/s), verify ${JSON.stringify(k.d.verify)}`);
  check("kernel: while downloading, it said it will be verified", k.seenReady);
  check("kernel: finished and verified against sha256sums.asc", k.d.status === "completed" && k.d.verify?.state === "verified" && k.d.verify.list === "sha256sums.asc", JSON.stringify(k.d.error));
  const kHex = await listed("https://cdn.kernel.org/pub/linux/kernel/v6.x/sha256sums.asc", "linux-6.10.tar.xz");
  check("kernel: the file on disk has the published SHA-256", (await digest(k.d.destination, "sha256")) === kHex);
  await page(`(() => { document.querySelector('[data-testid=nav-downloads]').click(); return true; })()`);
  await waitFor(() => has(`[data-testid=download-${k.d.id}] [data-testid=verify-badge][data-state=verified]`), "the Verified badge", 10000);
  check("kernel: the card says Verified", true);

  // ---- Debian: ~800 MB, verified with SHA-512
  const deb = await run(DEBIAN, 30);
  console.log(`debian: ${deb.d.status} in ${deb.secs.toFixed(0)} s, verify ${JSON.stringify(deb.d.verify)}`);
  check("Debian: finished and verified against SHA512SUMS", deb.d.status === "completed" && deb.d.verify?.state === "verified" && deb.d.verify.list === "SHA512SUMS" && deb.d.verify.algo === "SHA-512", JSON.stringify(deb.d.error));
  const dHex = await listed(`${DEBIAN_DIR}SHA512SUMS`, debName);
  check("Debian: the file on disk has the published SHA-512", (await digest(deb.d.destination, "sha512")) === dHex);

  // ---- not listed: finished as before
  const u = await run(UNLISTED, 3);
  check("a file its list does not name: finished, not verified", u.d.status === "completed" && !u.d.verify, JSON.stringify(u.d.verify));

  const hist = (await invoke("list_history", { limit: 10 })).ok;
  check("the History says which files were verified", hist.filter((x) => x.verify?.by === "publisher").length === 2, JSON.stringify(hist.map((x) => [x.filename, x.verify])));
  for (const x of [k, deb, u]) fs.rmSync(x.d.destination, { force: true });
} catch (e) {
  check(`unexpected error: ${e.message}`, false);
} finally {
  await h.cleanup();
  finish();
}
