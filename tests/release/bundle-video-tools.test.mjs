// scripts/bundle-video-tools.mjs: the programs packed into the installer are the official ones, checked, one entry
// out of an archive, compressed so they unpack to exactly the program.
//
//   node --test tests/release/bundle-video-tools.test.mjs
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import zlib from "node:zlib";
import { compress, parseChecksum, plan, releaseVersion, secure, zipEntry } from "../../scripts/bundle-video-tools.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const sha = (b) => createHash("sha256").update(b).digest("hex");

/** A zip with one deflated entry per file. */
function zipOf(files) {
  const parts = [];
  const central = [];
  let offset = 0;
  for (const [name, data] of files) {
    const body = zlib.deflateRawSync(data);
    const n = Buffer.from(name);
    const common = Buffer.alloc(26);
    common.writeUInt16LE(8, 4);
    common.writeUInt32LE(zlib.crc32(data) >>> 0, 10);
    common.writeUInt32LE(body.length, 14);
    common.writeUInt32LE(data.length, 18);
    common.writeUInt16LE(n.length, 22);
    const local = Buffer.concat([Buffer.from([0x50, 0x4b, 0x03, 0x04]), common, n, body]);
    const cen = Buffer.alloc(46);
    cen.writeUInt32LE(0x02014b50, 0);
    common.copy(cen, 6, 0, 26);
    cen.writeUInt32LE(offset, 42);
    central.push(Buffer.concat([cen, n]));
    parts.push(local);
    offset += local.length;
  }
  const cd = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(files.length, 8);
  end.writeUInt16LE(files.length, 10);
  end.writeUInt32LE(cd.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...parts, cd, end]);
}

test("the sources are the app's own: official https addresses and the same release files", () => {
  const tools = fs.readFileSync(path.join(ROOT, "crates", "download-manager", "src", "manager", "tools.rs"), "utf8");
  for (const target of [
    { os: "windows", arch: "x86_64" },
    { os: "windows", arch: "aarch64" },
    { os: "linux", arch: "x86_64" },
    { os: "linux", arch: "aarch64" },
    { os: "macos", arch: "aarch64" },
    { os: "macos", arch: "x86_64" },
  ]) {
    const p = plan(target);
    for (const t of [p.ytdlp, p.deno, p.ffmpeg]) {
      assert.ok(t, `${target.os} ${target.arch}`);
      assert.match(t.base, /^https:\/\/(github\.com\/yt-dlp\/|dl\.deno\.land|ffmpeg\.martin-riedl\.de\/)/);
      assert.ok(tools.includes(`"${t.base}"`), `${t.base} is a source the app knows`);
    }
    assert.ok(tools.includes(`"${p.ytdlp.asset}"`), p.ytdlp.asset);
    if (p.ffmpeg.archive) assert.ok(tools.includes(`"${p.ffmpeg.archive}"`), p.ffmpeg.archive);
  }
  assert.equal(plan({ os: "windows", arch: "x86_64" }).file("deno"), "deno.exe");
  assert.equal(plan({ os: "linux", arch: "x86_64" }).file("deno"), "deno");
  assert.throws(() => secure("http://example.com/x"), /not https/);
  assert.doesNotThrow(() => secure("http://127.0.0.1:9/x"));
});

test("checksums and versions are read like the app reads them", () => {
  const h = "a".repeat(64);
  assert.equal(parseChecksum(`${"1".repeat(64)}  other\n${h} *yt-dlp.exe\n`, "yt-dlp.exe"), h);
  assert.equal(parseChecksum(`${"1".repeat(64)}  other\n`, "yt-dlp.exe"), null);
  assert.equal(parseChecksum(`Hash : ${h.toUpperCase()}\r\n`, null), h);
  assert.equal(parseChecksum(`${h}\n${"b".repeat(64)}\n`, null), null, "two hashes, no name: refused");
  assert.equal(releaseVersion("https://github.com/yt-dlp/yt-dlp/releases/download/2025.10.22/yt-dlp.exe", "yt-dlp.exe"), "2025.10.22");
  assert.equal(releaseVersion("https://github.com/yt-dlp/yt-dlp/releases/download/2025.10.22/yt-dlp.exe", "yt-dlp_linux"), null);
  assert.equal(releaseVersion("https://github.com/yt-dlp/yt-dlp/releases/tag/2026.10.26", "yt-dlp.exe"), "2026.10.26");
  assert.equal(releaseVersion("https://release-assets.githubusercontent.com/github-production-release-asset/1/x?sp=r", "yt-dlp.exe"), null);
});

test("exactly one entry, by its fixed name, comes out of an archive", () => {
  const program = Buffer.from(Array.from({ length: 100_000 }, (_, i) => (i * 31) & 0xff));
  const zip = zipOf([["ffmpeg-x/README.txt", Buffer.from("hi")], ["ffmpeg-x/bin/ffmpeg.exe", program], ["../evil.exe", Buffer.from("x")]]);
  assert.deepEqual(zipEntry(zip, "bin/ffmpeg.exe"), program);
  assert.throws(() => zipEntry(zip, "fmpeg.exe"), /not in the archive/);
  assert.throws(() => zipEntry(zipOf([["a/deno", Buffer.from("1")], ["b/deno", Buffer.from("2")]]), "deno"), /twice/);
  const damaged = Buffer.from(zip);
  damaged[Math.floor(zip.length / 2)] ^= 0xff; // inside the program's compressed bytes
  assert.throws(() => zipEntry(damaged, "bin/ffmpeg.exe"));
});

test("a program is compressed so it unpacks to exactly itself", { skip: spawnSync("xz", ["--version"]).status !== 0 && "xz is not installed" }, () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "velox-bundle-"));
  const program = Buffer.from(Array.from({ length: 300_000 }, (_, i) => ((i * 2654435761) >>> 13) & 0xff));
  fs.writeFileSync(path.join(dir, "deno"), program);
  for (const arch of ["x86_64", "aarch64"]) {
    compress(path.join(dir, "deno"), path.join(dir, `deno-${arch}.xz`), arch);
    const back = spawnSync("xz", ["-dc", path.join(dir, `deno-${arch}.xz`)], { maxBuffer: 1 << 24 }).stdout;
    assert.equal(sha(back), sha(program), arch);
  }
  fs.rmSync(dir, { recursive: true, force: true });
});
