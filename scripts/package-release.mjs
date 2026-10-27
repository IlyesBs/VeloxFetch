#!/usr/bin/env node
// Collects a finished build into release/: the installer(s) for this system, the portable zip, the
// browser extension zips, and SHA256SUMS.txt over all of them. No dependencies (the zip writer is
// below), so it runs the same on Windows, macOS and Linux. scripts/build-release.sh runs it after the
// build; it can also be run on its own once `npm run release` (or the per-OS tauri build) has run.
//
//   node scripts/package-release.mjs
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import zlib from "node:zlib";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const REL = path.join(ROOT, "release");
const TARGET = path.join(ROOT, process.env.CARGO_TARGET_DIR ?? "target", "release");
const BUNDLE = path.join(TARGET, "bundle");
const EXT = path.join(ROOT, "apps", "browser-extension", "dist");
const os = process.platform === "win32" ? "windows" : process.platform === "darwin" ? "macos" : "linux";
const exe = os === "windows" ? ".exe" : "";
const arch = process.arch === "arm64" ? "arm64" : "x64";
const VERSION = JSON.parse(fs.readFileSync(path.join(ROOT, "apps", "desktop", "src-tauri", "tauri.conf.json"), "utf8")).version;
// Files of these kinds in release/ are artifacts: the old ones are replaced, all are hashed.
const SHIPPED = /\.(exe|msi|zip|dmg|deb|rpm|AppImage)$/;

const die = (msg) => {
  console.error(`package-release: ${msg}`);
  process.exit(1);
};

// ---- a minimal zip writer (deflate, zip64 not needed: every file is far below 4 GB) ----------------

const CRC_TABLE = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc32 =
  zlib.crc32 ??
  ((buf) => {
    let c = 0xffffffff;
    for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  });

function dosTime(d) {
  const time = (d.getHours() << 11) | (d.getMinutes() << 5) | (d.getSeconds() >> 1);
  const date = ((Math.max(d.getFullYear(), 1980) - 1980) << 9) | ((d.getMonth() + 1) << 5) | d.getDate();
  return { time, date };
}

/** entries: [{ name: "a/b.txt" (forward slashes), file?: absolute path, dir?: true }] */
function writeZip(out, entries) {
  const parts = [];
  const central = [];
  let offset = 0;
  for (const e of entries) {
    const name = Buffer.from(e.dir ? `${e.name.replace(/\/$/, "")}/` : e.name, "utf8");
    const st = e.file ? fs.statSync(e.file) : null;
    const data = e.dir ? Buffer.alloc(0) : fs.readFileSync(e.file);
    const packed = e.dir ? data : zlib.deflateRawSync(data, { level: 9 });
    const stored = !e.dir && packed.length >= data.length; // incompressible: store as is
    const body = stored ? data : packed;
    const method = e.dir || stored ? 0 : 8;
    const crc = e.dir ? 0 : crc32(data);
    const { time, date } = dosTime(st ? st.mtime : new Date());
    // Unix permissions in the high 16 bits so programs stay executable when unzipped on macOS / Linux.
    const mode = e.dir ? 0o40755 : e.exec || (st && st.mode & 0o111) ? 0o100755 : 0o100644;
    const ext = ((mode << 16) | (e.dir ? 0x10 : 0)) >>> 0;

    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(0x0800, 6); // UTF-8 names
    local.writeUInt16LE(method, 8);
    local.writeUInt16LE(time, 10);
    local.writeUInt16LE(date, 12);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(body.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(name.length, 26);
    local.writeUInt16LE(0, 28);
    parts.push(local, name, body);

    const cen = Buffer.alloc(46);
    cen.writeUInt32LE(0x02014b50, 0);
    cen.writeUInt16LE((3 << 8) | 20, 4); // made by Unix, spec 2.0
    cen.writeUInt16LE(20, 6);
    cen.writeUInt16LE(0x0800, 8);
    cen.writeUInt16LE(method, 10);
    cen.writeUInt16LE(time, 12);
    cen.writeUInt16LE(date, 14);
    cen.writeUInt32LE(crc, 16);
    cen.writeUInt32LE(body.length, 20);
    cen.writeUInt32LE(data.length, 24);
    cen.writeUInt16LE(name.length, 28);
    cen.writeUInt32LE(ext, 38);
    cen.writeUInt32LE(offset, 42);
    central.push(cen, name);
    offset += local.length + name.length + body.length;
  }
  const cenSize = central.reduce((n, b) => n + b.length, 0);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(cenSize, 12);
  end.writeUInt32LE(offset, 16);
  fs.writeFileSync(out, Buffer.concat([...parts, ...central, end]));
}

/** Every file and folder under `dir`, named `prefix/...`, sorted so the zip is reproducible in order. */
function tree(dir, prefix) {
  const out = prefix ? [{ name: prefix, dir: true }] : [];
  for (const e of fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    const p = path.join(dir, e.name);
    const name = prefix ? `${prefix}/${e.name}` : e.name;
    if (e.isSymbolicLink()) die(`refusing to pack a symbolic link: ${p}`);
    if (e.isDirectory()) out.push(...tree(p, name));
    else out.push({ name, file: p });
  }
  return out;
}

const sha256 = (file) => createHash("sha256").update(fs.readFileSync(file)).digest("hex");

// ---- collect -----------------------------------------------------------------------------------

for (const t of ["chrome", "chrome-store", "firefox"]) {
  if (!fs.existsSync(path.join(EXT, t, "manifest.json"))) die(`the extension is not built (missing ${path.join(EXT, t)}): run npm run build --prefix apps/browser-extension`);
}
const desktop = path.join(TARGET, `velox-desktop${exe}`);
const host = path.join(TARGET, `velox-native-host${exe}`);
for (const f of [desktop, host]) if (!fs.existsSync(f)) die(`missing ${f}: build the release first`);

fs.mkdirSync(REL, { recursive: true });
for (const n of fs.readdirSync(REL)) if (SHIPPED.test(n)) fs.rmSync(path.join(REL, n));

// 1. installers made by `tauri build`
const installers = { windows: ["nsis", "msi"], macos: ["dmg"], linux: ["deb", "rpm", "appimage"] }[os];
const made = [];
for (const kind of installers) {
  const d = path.join(BUNDLE, kind);
  if (!fs.existsSync(d)) continue;
  for (const n of fs.readdirSync(d)) {
    // only this version's installers (an older version's can linger in the bundle folder)
    if (!SHIPPED.test(n) || !n.includes(VERSION)) continue;
    fs.copyFileSync(path.join(d, n), path.join(REL, n));
    made.push(n);
  }
}
if (made.length === 0) die(`no installer found in ${BUNDLE}: did the tauri build run?`);

// 2. portable: the programs + the extension folder (+ the compressed video tools when the build packed them: the
// first start unpacks them, offline), kept together (the app looks beside itself)
const VIDEO_TOOLS = path.join(ROOT, process.env.CARGO_TARGET_DIR ?? "target", "video-tools");
const videoTools = (top) => (fs.existsSync(path.join(VIDEO_TOOLS, "video-tools.json")) ? tree(VIDEO_TOOLS, `${top}/video-tools`) : []);
const portableName = os === "windows" ? "VeloxFetch-portable.zip" : `VeloxFetch-portable-${os}-${arch}.zip`;
if (os === "macos") {
  const appDir = path.join(BUNDLE, "macos");
  const app = fs.existsSync(appDir) ? fs.readdirSync(appDir).find((n) => n.endsWith(".app")) : undefined;
  if (!app) die(`no .app in ${appDir}`);
  writeZip(path.join(REL, portableName), tree(path.join(appDir, app), app));
} else {
  const top = "VeloxFetch-portable";
  writeZip(path.join(REL, portableName), [
    { name: top, dir: true },
    { name: `${top}/velox-desktop${exe}`, file: desktop, exec: true },
    { name: `${top}/velox-native-host${exe}`, file: host, exec: true },
    { name: `${top}/extension`, dir: true },
    ...tree(path.join(EXT, "chrome"), `${top}/extension/chromium`),
    ...tree(path.join(EXT, "firefox"), `${top}/extension/firefox`),
    ...videoTools(top),
  ]);
}

// 3. the extension on its own (manifest.json at the top, as the stores and "Load unpacked" expect)
writeZip(path.join(REL, "VeloxFetch-extension-chromium.zip"), tree(path.join(EXT, "chrome"), ""));
writeZip(path.join(REL, "VeloxFetch-extension-chrome-store.zip"), tree(path.join(EXT, "chrome-store"), ""));
writeZip(path.join(REL, "VeloxFetch-extension-firefox.zip"), tree(path.join(EXT, "firefox"), ""));

// 4. checksums over everything shipped (two spaces: the format sha256sum -c / shasum -c read)
const shipped = fs.readdirSync(REL).filter((n) => SHIPPED.test(n)).sort();
fs.writeFileSync(path.join(REL, "SHA256SUMS.txt"), shipped.map((n) => `${sha256(path.join(REL, n))}  ${n}`).join("\n") + "\n");

console.log(`\nrelease/ (${os}-${arch}):`);
for (const n of shipped) console.log(`  ${n.padEnd(48)} ${(fs.statSync(path.join(REL, n)).size / 1048576).toFixed(1).padStart(7)} MB`);
console.log("  SHA256SUMS.txt");
