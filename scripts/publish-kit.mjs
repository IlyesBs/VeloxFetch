#!/usr/bin/env node
// The production publishing kit (docs/publishing.md). `scripts/publish-release.sh` runs it; it can also be run alone.
// No dependencies: Node's own fs and crypto, the same on Windows, macOS and Linux.
//
//   node scripts/publish-kit.mjs preflight --key <update key file> [--allow-dirty]
//       Is this tree fit to become a public release? One version everywhere, the update key trusted by the app and
//       present on this computer, nothing uncommitted. Prints the version.
//   node scripts/publish-kit.mjs check-binary [--app <path>]
//       The built app really trusts the update key(s) of update-keys.txt (they are compiled in), and is a release build.
//   node scripts/publish-kit.mjs stage --version <x.y.z> --out <folder> --from <release folder> [--from <folder>]...
//       Copies every package of every system's release folder into <out>/files (each checked against its own
//       SHA256SUMS.txt first), writes one SHA256SUMS.txt over all of them, the release notes, and <out>/repo: the
//       README and screenshots for the public download repository. Refuses unknown, mismatched or mixed-version files.
//
// Nothing here signs, uploads or reads a secret: the passphrase is only ever typed into scripts/update-key.mjs.

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const PUBLISH_DOCS = path.join(ROOT, "docs", "publish");
const KEYS_FILE = path.join(ROOT, "apps", "desktop", "src-tauri", "update-keys.txt");
const SECRET_FORMAT = "velox-update-secret-key-v1";

/** Every file a release may ship: [pattern, system, what it is]. `V` is the version (x.y.z). Nothing else is published. */
const SHIPPED = [
  [/^VeloxFetch_(?<v>\d+\.\d+\.\d+)_x64-setup\.exe$/, "Windows", "Installer (64-bit)"],
  [/^VeloxFetch_(?<v>\d+\.\d+\.\d+)_arm64-setup\.exe$/, "Windows", "Installer (ARM)"],
  [/^VeloxFetch_(?<v>\d+\.\d+\.\d+)_x64_[a-z]{2}-[A-Z]{2}\.msi$/, "Windows", "MSI package (for IT deployment)"],
  [/^VeloxFetch-portable\.zip$/, "Windows", "Portable (no install)"],
  [/^VeloxFetch_(?<v>\d+\.\d+\.\d+)_(aarch64|x64)\.dmg$/, "macOS", "Disk image"],
  [/^VeloxFetch-portable-macos-(arm64|x64)\.zip$/, "macOS", "Portable app (zip)"],
  [/^VeloxFetch_(?<v>\d+\.\d+\.\d+)_(amd64|aarch64)\.AppImage$/, "Linux", "AppImage (any distribution)"],
  [/^VeloxFetch_(?<v>\d+\.\d+\.\d+)_(amd64|arm64)\.deb$/, "Linux", "Debian / Ubuntu / Mint (.deb)"],
  [/^VeloxFetch-(?<v>\d+\.\d+\.\d+)-1\.(x86_64|aarch64)\.rpm$/, "Linux", "Fedora / openSUSE (.rpm)"],
  [/^VeloxFetch-portable-linux-(x64|arm64)\.zip$/, "Linux", "Portable (zip)"],
  [/^VeloxFetch-extension-chromium\.zip$/, "Browser", "Extension for Chrome, Edge, Brave (load unpacked)"],
  [/^VeloxFetch-extension-chrome-store\.zip$/, "Browser", "Extension package for the Chrome / Edge stores"],
  [/^VeloxFetch-extension-firefox\.zip$/, "Browser", "Extension for Firefox"],
];
/** Built once per system from the same sources: the first folder's copy is the one published. */
const SAME_EVERYWHERE = /^VeloxFetch-extension-/;
const ARTIFACT = /\.(exe|msi|zip|dmg|deb|rpm|AppImage)$/;
const SYSTEMS = ["Windows", "macOS", "Linux", "Browser"];

class Refusal extends Error {}
const fail = (msg) => {
  throw new Refusal(msg);
};

function args(argv) {
  const out = { _: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (!a.startsWith("--")) {
      out._.push(a);
      continue;
    }
    const k = a.slice(2);
    if (k === "allow-dirty") {
      out[k] = true;
      continue;
    }
    const v = argv[++i];
    if (v === undefined || v.startsWith("--")) fail(`--${k} needs a value`);
    (out[k] ??= []).push(v);
  }
  return out;
}
const one = (a, k) => (a[k] ? a[k][a[k].length - 1] : undefined);

export function sha256File(file) {
  const h = createHash("sha256");
  const fd = fs.openSync(file, "r");
  try {
    const buf = Buffer.alloc(1 << 20);
    for (let n; (n = fs.readSync(fd, buf, 0, buf.length, null)) > 0; ) h.update(buf.subarray(0, n));
  } finally {
    fs.closeSync(fd);
  }
  return h.digest("hex");
}

/** `SHA256SUMS.txt` as name -> hash. */
export function readSums(file) {
  const sums = new Map();
  for (const line of fs.readFileSync(file, "utf8").split(/\r?\n/).filter(Boolean)) {
    const m = line.match(/^([0-9a-f]{64})\s+\*?(.+)$/);
    if (!m) fail(`${file}: not a checksum line: ${line}`);
    sums.set(m[2], m[1]);
  }
  return sums;
}

/** The public keys the app trusts (hex), from update-keys.txt. */
export function trustedKeys(file = KEYS_FILE) {
  return fs
    .readFileSync(file, "utf8")
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l && !l.startsWith("#"))
    .map((l) => {
      const m = l.match(/^velox-update-key-v1 ([0-9a-f]{64})$/);
      if (!m) fail(`${path.relative(ROOT, file)}: not a public key line: ${l}`);
      return m[1];
    });
}

/** The version every file that carries one agrees on (the policy test "one version everywhere" checks the rest). */
export function appVersion(root = ROOT) {
  const conf = JSON.parse(fs.readFileSync(path.join(root, "apps", "desktop", "src-tauri", "tauri.conf.json"), "utf8")).version;
  const pkg = JSON.parse(fs.readFileSync(path.join(root, "apps", "desktop", "package.json"), "utf8")).version;
  const cargo = fs.readFileSync(path.join(root, "Cargo.toml"), "utf8").match(/^\[workspace\.package\][^[]*?^version\s*=\s*"([^"]+)"/m)?.[1];
  for (const [where, v] of [["apps/desktop/package.json", pkg], ["Cargo.toml", cargo]]) {
    if (v !== conf) fail(`${where} says ${v}, tauri.conf.json says ${conf}: one version everywhere first`);
  }
  if (!/^(0|[1-9]\d{0,5})\.(0|[1-9]\d{0,5})\.(0|[1-9]\d{0,5})$/.test(conf)) fail(`version ${conf} is not x.y.z (the updater refuses anything else)`);
  return conf;
}

export function preflight({ key, allowDirty = false, root = ROOT, keysFile = KEYS_FILE }) {
  const version = appVersion(root);
  const keys = trustedKeys(keysFile);
  if (keys.length === 0) {
    fail(
      "no update key yet: a release built now could never update itself.\n" +
        "  Make it once, in your own terminal:  node scripts/update-key.mjs generate\n" +
        "  then commit apps/desktop/src-tauri/update-keys.txt and run this again (docs/auto-update.md).",
    );
  }
  if (!key) fail("--key <update key file> is needed (the encrypted key made by update-key.mjs generate)");
  if (!fs.existsSync(key)) fail(`the update key file ${key} is not there (plug in the drive that holds it, or pass --key)`);
  let doc;
  try {
    doc = JSON.parse(fs.readFileSync(key, "utf8"));
  } catch {
    fail(`${key} is not an update key file`);
  }
  if (doc?.format !== SECRET_FORMAT) fail(`${key} is not an update key file`);
  const keyFileInRepo = path.relative(root, path.resolve(key));
  if (!keyFileInRepo.startsWith("..") && !path.isAbsolute(keyFileInRepo)) fail("the update key file must never be inside the repository");
  // The key's id is the first 8 bytes of SHA-256 of the public key (update-key.mjs): it must be one the app trusts.
  const ids = keys.map((k) => createHash("sha256").update(Buffer.from(k, "hex")).digest().subarray(0, 8).toString("hex"));
  if (!ids.includes(doc.keyId)) fail(`the key in ${key} (${doc.keyId}) is not in update-keys.txt: the app would refuse its signatures`);
  if (!allowDirty) {
    const dirty = execFileSync("git", ["status", "--porcelain", "--untracked-files=no"], { cwd: root, encoding: "utf8" }).trim();
    if (dirty) fail(`uncommitted changes: a public release is built from a commit.\n${dirty}`);
  }
  return { version, keyId: doc.keyId };
}

/** The built app has the trusted keys compiled in (updates work) and is not a debug build. */
export function checkBinary(app, keysFile = KEYS_FILE) {
  if (!fs.existsSync(app)) fail(`${app} is not built`);
  const bin = fs.readFileSync(app);
  for (const k of trustedKeys(keysFile)) {
    if (!bin.includes(Buffer.from(k))) fail(`${app} does not carry the update key ${k.slice(0, 16)}…: build again after committing update-keys.txt`);
  }
  // Debug-only test switches are compiled out of release builds (policy test); their names must not be in the binary.
  for (const marker of ["VELOX_UPDATE_FEED", "VELOX_DRY_RUN_SHELL"]) {
    if (bin.includes(Buffer.from(marker))) fail(`${app} is a debug build (it knows ${marker}): build the release with build-release.sh`);
  }
}

/** What a published file is: { system, what } or null. Also refuses a package of another version. */
export function classify(name, version) {
  for (const [re, system, what] of SHIPPED) {
    const m = name.match(re);
    if (!m) continue;
    if (m.groups?.v && m.groups.v !== version) fail(`${name} is version ${m.groups.v}, not ${version}`);
    return { system, what };
  }
  return null;
}

const mib = (n) => `${(n / 1048576).toFixed(1)} MB`;

export function stage({ version, out, from }) {
  if (!from?.length) fail("--from <release folder> is needed");
  const files = path.join(out, "files");
  // Only an earlier kit is ever replaced: a folder holding anything else is refused, never emptied.
  if (fs.existsSync(out) && fs.readdirSync(out).length > 0 && !fs.existsSync(path.join(out, "RELEASE_NOTES.md"))) {
    fail(`${out} is not empty and is not an earlier publishing kit: choose another --out`);
  }
  fs.rmSync(out, { recursive: true, force: true });
  fs.mkdirSync(files, { recursive: true });
  const picked = new Map(); // name -> { system, what, sha, size }
  const notes = [];
  for (const dir of from) {
    const sumsFile = path.join(dir, "SHA256SUMS.txt");
    if (!fs.existsSync(sumsFile)) fail(`${dir} has no SHA256SUMS.txt: is it a release folder made by build-release.sh?`);
    const sums = readSums(sumsFile);
    for (const name of fs.readdirSync(dir).sort()) {
      const file = path.join(dir, name);
      if (!fs.statSync(file).isFile() || !ARTIFACT.test(name)) continue;
      const kind = classify(name, version);
      if (!kind) fail(`${path.join(dir, name)}: not a file a release ships (unknown name)`);
      if (fs.lstatSync(file).isSymbolicLink()) fail(`${file} is a symbolic link`);
      const sha = sha256File(file);
      if (sums.get(name) !== sha) fail(`${file} does not match ${sumsFile}: damaged, or changed after the build`);
      const had = picked.get(name);
      if (had) {
        if (had.sha === sha) continue;
        if (SAME_EVERYWHERE.test(name)) {
          notes.push(`${name}: the copy from ${had.dir} is published (built on every system from the same sources)`);
          continue;
        }
        fail(`${name} is in two release folders with different contents`);
      }
      picked.set(name, { ...kind, sha, size: fs.statSync(file).size, dir });
      fs.copyFileSync(file, path.join(files, name));
    }
    for (const name of sums.keys()) {
      if (!fs.existsSync(path.join(dir, name))) fail(`${sumsFile} lists ${name}, which is missing`);
    }
    // The software bill of materials (what the programs are made of), for companies that ask for it.
    const sbom = path.join(dir, "sbom");
    if (fs.existsSync(sbom)) {
      for (const n of fs.readdirSync(sbom).filter((n) => /^[a-z0-9-]+\.cdx\.json$/.test(n)).sort()) {
        const target = `VeloxFetch-${version}-sbom-${n}`;
        if (!fs.existsSync(path.join(files, target))) fs.copyFileSync(path.join(sbom, n), path.join(files, target));
      }
    }
  }
  const installers = [...picked.values()].filter((p) => p.system !== "Browser");
  if (installers.length === 0) fail("no installer found in the release folders");

  const names = fs.readdirSync(files).sort();
  fs.writeFileSync(path.join(files, "SHA256SUMS.txt"), names.map((n) => `${sha256File(path.join(files, n))}  ${n}`).join("\n") + "\n");

  // The downloads table (release notes and README), by system.
  const table = ["| System | File | What it is | Size |", "|---|---|---|---|"];
  for (const system of SYSTEMS) {
    for (const [name, p] of [...picked].filter(([, p]) => p.system === system)) {
      table.push(`| ${system} | \`${name}\` | ${p.what} | ${mib(p.size)} |`);
    }
  }
  const missing = ["Windows", "macOS", "Linux"].filter((s) => !installers.some((p) => p.system === s));
  const fill = (text) =>
    text
      .replaceAll("{{VERSION}}", version)
      .replaceAll("{{DOWNLOADS}}", table.join("\n"))
      .replaceAll("{{SYSTEMS}}", ["Windows", "macOS", "Linux"].filter((s) => !missing.includes(s)).join(", "));
  const template = (n) => fs.readFileSync(path.join(PUBLISH_DOCS, n), "utf8");
  fs.writeFileSync(path.join(out, "RELEASE_NOTES.md"), fill(template("release-notes.md")));

  // The public repository's front page and its pictures, ready to copy in.
  const repo = path.join(out, "repo");
  fs.mkdirSync(path.join(repo, "screenshots"), { recursive: true });
  fs.writeFileSync(path.join(repo, "README.md"), fill(template("README-public.md")));
  const shots = path.join(PUBLISH_DOCS, "screenshots");
  const wanted = [...template("README-public.md").matchAll(/\(screenshots\/([a-z0-9-]+\.png)\)/g)].map((m) => m[1]);
  for (const n of wanted) {
    if (!fs.existsSync(path.join(shots, n))) fail(`README-public.md shows screenshots/${n}, which is not in docs/publish/screenshots`);
    fs.copyFileSync(path.join(shots, n), path.join(repo, "screenshots", n));
  }
  fs.writeFileSync(path.join(out, "LISTINGS.md"), fill(template("listings.md")));
  return { files: names, missing, notes, picked };
}

async function main() {
  const a = args(process.argv.slice(2));
  const cmd = a._[0];
  if (cmd === "preflight") {
    const { version, keyId } = preflight({ key: one(a, "key"), allowDirty: !!a["allow-dirty"] });
    console.log(`version ${version}, update key ${keyId}`);
    console.log(`VERSION=${version}`);
  } else if (cmd === "check-binary") {
    const app = one(a, "app") ?? path.join(ROOT, "target", "release", process.platform === "win32" ? "velox-desktop.exe" : "velox-desktop");
    checkBinary(app);
    console.log(`${path.relative(ROOT, app)}: release build, trusts the update key(s) of update-keys.txt`);
  } else if (cmd === "stage") {
    const version = one(a, "version");
    if (!version) fail("--version x.y.z is needed");
    const out = path.resolve(one(a, "out") ?? path.join(ROOT, "release", "publish", `v${version}`));
    const r = stage({ version, out, from: (a.from ?? []).map((d) => path.resolve(d)) });
    console.log(`${out}:`);
    for (const n of r.files) console.log(`  files/${n}`);
    console.log("  RELEASE_NOTES.md, LISTINGS.md, repo/README.md, repo/screenshots/");
    for (const n of r.notes) console.log(`  note: ${n}`);
    if (r.missing.length) console.log(`  not in this release yet: ${r.missing.join(", ")} (build there, then --merge its release folder)`);
  } else {
    console.error("usage: node scripts/publish-kit.mjs preflight|check-binary|stage (see the top of this file)");
    process.exit(2);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((e) => {
    console.error(`publish-kit: ${e instanceof Refusal ? e.message : e.stack ?? e}`);
    process.exit(1);
  });
}
