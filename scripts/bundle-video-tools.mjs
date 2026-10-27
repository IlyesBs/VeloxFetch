#!/usr/bin/env node
// Packs the programs video sites need (yt-dlp, Deno, ffmpeg) into the installer, so installing VeloxFetch
// downloads nothing more (users on mobile data paid ~300 MB for them before).
//
//   node scripts/bundle-video-tools.mjs [--out target/video-tools] [--os windows|linux|macos] [--arch x86_64|aarch64]
//
// Each program is fetched once, here, from its official server (https only), checked against the SHA-256 its
// publisher lists, and only the program itself is kept (out of an archive: one entry, by a fixed name; never a
// path from the archive). It is then compressed on its own with xz -9e behind the branch filter for its processor
// (executables shrink more). video-tools.json lists each one's version, size and SHA-256: the app unpacks and checks
// them with no network (crates/download-manager/src/manager/tools.rs, the same sources as ToolSources::official).
// The last line printed is the SHA-256 of video-tools.json: scripts/build-release.sh compiles it into the app, so a
// list changed on disk is refused.
//
// No dependencies (Node 20+, and the xz program). A failure exits non-zero: the release is then built without the
// copies and the app downloads the programs as before.
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { Readable, Transform } from "node:stream";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";
import zlib from "node:zlib";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
/** A program is never larger than this (the app's own limit is the same). */
const MAX_PROGRAM = 600 * 1024 * 1024;
const MAX_TEXT = 256 * 1024;

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

const die = (msg) => {
  console.error(`bundle-video-tools: ${msg}`);
  process.exit(1);
};

/** This computer, as the app names it (Rust's std::env::consts::OS / ARCH). */
export function hostTarget() {
  const os = process.platform === "win32" ? "windows" : process.platform === "darwin" ? "macos" : "linux";
  const arch = process.arch === "arm64" ? "aarch64" : process.arch === "x64" ? "x86_64" : process.arch;
  return { os, arch };
}

/** The official sources (keep in step with ToolSources::official and the *_asset functions in tools.rs). */
export function plan({ os, arch }) {
  const ytdlp = {
    windows: { x86_64: "yt-dlp.exe", aarch64: "yt-dlp_arm64.exe" },
    macos: { x86_64: "yt-dlp_macos", aarch64: "yt-dlp_macos" },
    linux: { x86_64: "yt-dlp_linux", aarch64: "yt-dlp_linux_aarch64" },
  }[os]?.[arch];
  const denoOs = { windows: "pc-windows-msvc", macos: "apple-darwin", linux: "unknown-linux-gnu" }[os];
  const exe = os === "windows" ? ".exe" : "";
  const ffmpeg =
    os === "windows"
      ? { archive: { x86_64: "ffmpeg-master-latest-win64-gpl.zip", aarch64: "ffmpeg-master-latest-winarm64-gpl.zip" }[arch], entry: "bin/ffmpeg.exe" }
      : { platform: { "macos-aarch64": "macos/arm64", "macos-x86_64": "macos/amd64", "linux-x86_64": "linux/amd64", "linux-aarch64": "linux/arm64" }[`${os}-${arch}`], entry: "ffmpeg" };
  return {
    file: (name) => `${name}${exe}`,
    ytdlp: ytdlp && { base: "https://github.com/yt-dlp/yt-dlp/releases/latest/download", asset: ytdlp },
    deno: denoOs && ["x86_64", "aarch64"].includes(arch) && { base: "https://dl.deno.land", asset: `deno-${arch}-${denoOs}.zip`, entry: `deno${exe}` },
    ffmpeg: os === "windows" ? ffmpeg.archive && { base: "https://github.com/yt-dlp/FFmpeg-Builds/releases/latest/download", ...ffmpeg } : ffmpeg.platform && { base: "https://ffmpeg.martin-riedl.de/redirect/latest", ...ffmpeg },
  };
}

/** Only encrypted connections (plain http only for a local test server). */
export function secure(url) {
  const u = new URL(url);
  const local = u.hostname === "127.0.0.1" || u.hostname === "localhost";
  if (u.protocol !== "https:" && !(u.protocol === "http:" && local)) throw new Error(`refusing a source that is not https: ${u.origin}`);
  return u;
}

/** The SHA-256 in a published checksum file: the line for `name` in a list, or the only hash in the file. */
export function parseChecksum(text, name) {
  if (name) {
    for (const line of text.split(/\r?\n/)) {
      const parts = line.trim().split(/\s+/);
      if (parts.length >= 2 && /^[0-9a-fA-F]{64}$/.test(parts[0]) && parts.at(-1).replace(/^\*/, "") === name) return parts[0].toLowerCase();
    }
    return null;
  }
  const all = text.match(/(?<![0-9a-fA-F])[0-9a-fA-F]{64}(?![0-9a-fA-F])/g) ?? [];
  return all.length === 1 ? all[0].toLowerCase() : null;
}

/** The version in a GitHub release download address: `.../releases/download/<version>/<asset>`. */
export function releaseVersion(finalUrl, asset) {
  const parts = new URL(finalUrl).pathname.split("/");
  const n = parts.length;
  return n >= 3 && parts[n - 1] === asset && parts[n - 3] === "download" && /^\d+(\.\d+)*$/.test(parts[n - 2]) ? parts[n - 2] : null;
}

/** The one entry of a zip named `wanted` (or ending in `/wanted`): its bytes, size and CRC checked. */
export function zipEntry(zip, wanted) {
  const tail = zip.subarray(Math.max(0, zip.length - 65_557));
  let eocd = -1;
  for (let i = tail.length - 22; i >= 0; i--) if (tail.readUInt32LE(i) === 0x06054b50) { eocd = i; break; }
  if (eocd < 0) throw new Error("not a zip file");
  const entries = tail.readUInt16LE(eocd + 10);
  const cdOffset = tail.readUInt32LE(eocd + 16);
  const want = wanted.toLowerCase();
  const found = [];
  let p = cdOffset;
  for (let k = 0; k < entries; k++) {
    if (zip.readUInt32LE(p) !== 0x02014b50) throw new Error("damaged zip directory");
    const [method, crc, csize, size] = [zip.readUInt16LE(p + 10), zip.readUInt32LE(p + 16), zip.readUInt32LE(p + 20), zip.readUInt32LE(p + 24)];
    const [n, e, c, local] = [zip.readUInt16LE(p + 28), zip.readUInt16LE(p + 30), zip.readUInt16LE(p + 32), zip.readUInt32LE(p + 42)];
    const name = zip.subarray(p + 46, p + 46 + n).toString("utf8").replace(/\\/g, "/").toLowerCase();
    if (name === want || name.endsWith(`/${want}`)) found.push({ method, crc, csize, size, local });
    p += 46 + n + e + c;
  }
  if (found.length !== 1) throw new Error(found.length ? "the archive holds the program twice" : "the program is not in the archive");
  const { method, crc, csize, size, local } = found[0];
  if (size > MAX_PROGRAM || ![0, 8].includes(method)) throw new Error("unsupported or oversized entry");
  if (zip.readUInt32LE(local) !== 0x04034b50) throw new Error("damaged zip entry");
  const start = local + 30 + zip.readUInt16LE(local + 26) + zip.readUInt16LE(local + 28);
  const raw = zip.subarray(start, start + csize);
  const data = method === 8 ? zlib.inflateRawSync(raw) : Buffer.from(raw);
  if (data.length !== size || crc32(data) >>> 0 !== crc >>> 0) throw new Error("the extracted program is damaged");
  return data;
}

async function getText(url) {
  const res = await fetch(secure(url), { redirect: "follow" });
  if (!res.ok) throw new Error(`${url}: HTTP ${res.status}`);
  secure(res.url);
  const text = await res.text();
  if (text.length > MAX_TEXT) throw new Error(`${url}: checksum file too large`);
  return { text, finalUrl: res.url };
}

/** Download `url` to `dest`; its SHA-256 and the address it ended at. */
async function getFile(url, dest) {
  const res = await fetch(secure(url), { redirect: "follow" });
  if (!res.ok || !res.body) throw new Error(`${url}: HTTP ${res.status}`);
  secure(res.url);
  const hash = createHash("sha256");
  let got = 0;
  const counted = new Transform({
    transform(chunk, _enc, done) {
      got += chunk.length;
      if (got > MAX_PROGRAM) return done(new Error(`${url}: unexpectedly large`));
      hash.update(chunk);
      done(null, chunk);
    },
  });
  await pipeline(Readable.fromWeb(res.body), counted, fs.createWriteStream(dest));
  return { sha256: hash.digest("hex"), finalUrl: res.url };
}

const sha256Of = (buf) => createHash("sha256").update(buf).digest("hex");

/** xz -9e behind the branch filter for `arch`, then unpacked again and compared (what the app will do). */
export function compress(input, output, arch) {
  const bcj = arch === "aarch64" ? "--arm64" : arch === "x86_64" ? "--x86" : null;
  const run = (filters) => {
    const out = fs.openSync(output, "w");
    try {
      return spawnSync("xz", ["--format=xz", "--check=crc64", ...filters, "--lzma2=preset=9e", "-T1", "-c", input], { stdio: ["ignore", out, "inherit"] });
    } finally {
      fs.closeSync(out);
    }
  };
  let r = run(bcj ? [bcj] : []);
  if (r.status !== 0 && bcj) r = run([]); // an xz too old for this filter: plain LZMA2
  if (r.error || r.status !== 0) throw new Error(`xz failed (${r.error?.message ?? `exit ${r.status}`}): install xz (xz-utils)`);
  const back = spawnSync("xz", ["-dc", output], { maxBuffer: MAX_PROGRAM + 1 });
  if (back.status !== 0 || sha256Of(back.stdout) !== sha256Of(fs.readFileSync(input))) throw new Error(`${output}: does not unpack to the program`);
}

async function main() {
  const args = process.argv.slice(2);
  const opt = (name, dflt) => {
    const i = args.indexOf(name);
    return i >= 0 ? args[i + 1] : dflt;
  };
  const host = hostTarget();
  const target = { os: opt("--os", host.os), arch: opt("--arch", host.arch) };
  const out = path.resolve(ROOT, opt("--out", path.join("target", "video-tools")));
  if (spawnSync("xz", ["--version"]).status !== 0) die("the xz program is needed (Debian / Ubuntu: xz-utils, Fedora: xz, macOS: brew install xz; Git for Windows has it)");
  const p = plan(target);
  const work = fs.mkdtempSync(path.join(fs.realpathSync(path.join(ROOT, "target")), "video-tools-work-"));
  fs.rmSync(out, { recursive: true, force: true });
  fs.mkdirSync(out, { recursive: true });
  const today = new Date().toISOString().slice(0, 10).replace(/-/g, ".");
  const tools = [];
  const add = (tool, name, program, version) => {
    const file = path.join(work, p.file(name));
    fs.writeFileSync(file, program);
    console.log(`${tool}: ${version}, ${(program.length / 1048576).toFixed(1)} MB, compressing...`);
    compress(file, path.join(out, `${p.file(name)}.xz`), target.arch);
    const packed = fs.statSync(path.join(out, `${p.file(name)}.xz`)).size;
    console.log(`${tool}: ${(packed / 1048576).toFixed(1)} MB in the installer`);
    tools.push({ tool, version, size: program.length, sha256: sha256Of(program), packed });
  };
  try {
    if (p.ytdlp) {
      const { text } = await getText(`${p.ytdlp.base}/SHA2-256SUMS`);
      const want = parseChecksum(text, p.ytdlp.asset) ?? die("no checksum published for yt-dlp");
      const { sha256, finalUrl } = await getFile(`${p.ytdlp.base}/${p.ytdlp.asset}`, path.join(work, "ytdlp.download"));
      if (sha256 !== want) die("yt-dlp does not match its published checksum");
      add("ytdlp", "yt-dlp", fs.readFileSync(path.join(work, "ytdlp.download")), releaseVersion(finalUrl, p.ytdlp.asset) ?? today);
    }
    if (p.deno) {
      const version = (await getText(`${p.deno.base}/release-latest.txt`)).text.trim();
      if (!/^v\d+(\.\d+)*$/.test(version)) die(`unexpected Deno version ${version}`);
      const want = parseChecksum((await getText(`${p.deno.base}/release/${version}/${p.deno.asset}.sha256sum`)).text, null) ?? die("no checksum published for Deno");
      const zip = path.join(work, "deno.zip");
      if ((await getFile(`${p.deno.base}/release/${version}/${p.deno.asset}`, zip)).sha256 !== want) die("Deno does not match its published checksum");
      add("deno", "deno", zipEntry(fs.readFileSync(zip), p.deno.entry), version.slice(1));
    }
    if (p.ffmpeg) {
      const zip = path.join(work, "ffmpeg.zip");
      let want;
      if (p.ffmpeg.archive) {
        want = parseChecksum((await getText(`${p.ffmpeg.base}/checksums.sha256`)).text, p.ffmpeg.archive);
        const got = await getFile(`${p.ffmpeg.base}/${p.ffmpeg.archive}`, zip);
        if (!want || got.sha256 !== want) die("ffmpeg does not match its published checksum");
      } else {
        // `latest` redirects to the current build; its checksum sits beside it.
        const got = await getFile(`${p.ffmpeg.base}/${p.ffmpeg.platform}/release/ffmpeg.zip`, zip);
        want = parseChecksum((await getText(`${got.finalUrl}.sha256`)).text, null);
        if (!want || got.sha256 !== want) die("ffmpeg does not match its published checksum");
      }
      add("ffmpeg", "ffmpeg", zipEntry(fs.readFileSync(zip), p.ffmpeg.entry), today);
    }
  } catch (e) {
    die(e.message);
  } finally {
    fs.rmSync(work, { recursive: true, force: true });
  }
  if (tools.length === 0) die(`no program for ${target.os} ${target.arch}`);
  const manifest = { format: 1, os: target.os, arch: target.arch, created: today, tools };
  const text = `${JSON.stringify(manifest, null, 2)}\n`;
  fs.writeFileSync(path.join(out, "video-tools.json"), text);
  fs.writeFileSync(
    path.join(out, "THIRD-PARTY-NOTICES.txt"),
    [
      "Programs VeloxFetch installs for video sites, unchanged, as their publishers build them:",
      "",
      "yt-dlp  - The Unlicense (public domain)            - https://github.com/yt-dlp/yt-dlp",
      "Deno    - MIT License                              - https://github.com/denoland/deno",
      "FFmpeg  - GNU General Public License v3 or later   - https://ffmpeg.org (source: https://git.ffmpeg.org/ffmpeg.git;",
      "          builds: https://github.com/yt-dlp/FFmpeg-Builds, https://ffmpeg.martin-riedl.de)",
      "",
    ].join("\n"),
  );
  const total = tools.reduce((n, t) => n + t.packed, 0);
  const unpacked = tools.reduce((n, t) => n + t.size, 0);
  console.log(`video tools: ${(unpacked / 1048576).toFixed(0)} MB of programs, ${(total / 1048576).toFixed(0)} MB in the installer (${out})`);
  console.log(`VELOX_VIDEO_TOOLS_SHA256=${sha256Of(Buffer.from(text))}`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await main();
