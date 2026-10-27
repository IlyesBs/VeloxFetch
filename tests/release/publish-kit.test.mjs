// The production publishing kit (scripts/publish-kit.mjs, scripts/publish-release.sh; docs/publishing.md).
//
//   node --test tests/release/publish-kit.test.mjs
//
// Unit checks of what the kit refuses, then the whole publish script run for real in a throwaway copy of the project:
// a throwaway update key is made and typed at a pseudo-terminal like the owner does, packages are staged, the manifest
// is signed and verified. Never touches the real key, the real release folder or the network.

import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { appVersion, checkBinary, classify, preflight, readSums, render, stage, writePage } from "../../scripts/publish-kit.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const V = appVersion();
const sha = (b) => createHash("sha256").update(b).digest("hex");
const tmp = (name) => fs.mkdtempSync(path.join(os.tmpdir(), `velox-publish-${name}-`));
const throwsMsg = (fn, re) => assert.throws(fn, (e) => re.test(e.message), `expected a refusal matching ${re}`);

/** A release folder like build-release.sh makes: the files, and SHA256SUMS.txt over them. */
function releaseDir(files, { sbom = false } = {}) {
  const dir = tmp("rel");
  for (const [name, body] of Object.entries(files)) fs.writeFileSync(path.join(dir, name), body);
  fs.writeFileSync(path.join(dir, "SHA256SUMS.txt"), Object.entries(files).map(([n, b]) => `${sha(b)}  ${n}`).join("\n") + "\n");
  if (sbom) {
    fs.mkdirSync(path.join(dir, "sbom"));
    fs.writeFileSync(path.join(dir, "sbom", "velox-desktop.cdx.json"), "{}");
  }
  return dir;
}
const linux = () => ({
  [`VeloxFetch_${V}_amd64.AppImage`]: randomBytes(4096),
  [`VeloxFetch_${V}_amd64.deb`]: randomBytes(2048),
  [`VeloxFetch-${V}-1.x86_64.rpm`]: randomBytes(2048),
  "VeloxFetch-portable-linux-x64.zip": randomBytes(1024),
  "VeloxFetch-extension-chromium.zip": Buffer.from("ext-linux"),
});

test("only the files a release ships are published, each of this version", () => {
  assert.equal(classify(`VeloxFetch_${V}_x64-setup.exe`, V).system, "Windows");
  assert.equal(classify(`VeloxFetch_${V}_aarch64.dmg`, V).system, "macOS");
  assert.equal(classify("VeloxFetch-extension-firefox.zip", V).system, "Browser");
  assert.equal(classify("notes.zip", V), null);
  assert.equal(classify("velox-desktop.exe", V), null);
  throwsMsg(() => classify("VeloxFetch_0.9.0_amd64.deb", V), /version 0\.9\.0, not/);
});

test("staging collects every system's packages, checked, with one checksum list, the notes and the repository page", () => {
  const win = releaseDir({ [`VeloxFetch_${V}_x64-setup.exe`]: randomBytes(4096), "VeloxFetch-portable.zip": randomBytes(1024), "VeloxFetch-extension-chromium.zip": Buffer.from("ext-win") });
  const lin = releaseDir(linux(), { sbom: true });
  const out = path.join(tmp("out"), "kit");
  const r = stage({ version: V, out, from: [lin, win] });
  const files = fs.readdirSync(path.join(out, "files")).sort();
  for (const n of [`VeloxFetch_${V}_x64-setup.exe`, `VeloxFetch_${V}_amd64.AppImage`, "VeloxFetch-portable.zip", "SHA256SUMS.txt", `VeloxFetch-${V}-sbom-velox-desktop.cdx.json`]) {
    assert.ok(files.includes(n), `${n} staged`);
  }
  // The extension is built on every system from the same sources: the first folder's copy, said so.
  assert.equal(fs.readFileSync(path.join(out, "files", "VeloxFetch-extension-chromium.zip"), "utf8"), "ext-linux");
  assert.ok(r.notes.some((n) => n.startsWith("VeloxFetch-extension-chromium.zip")));
  assert.deepEqual(r.missing, ["macOS"]);
  // One checksum line per published file, each right.
  const sums = readSums(path.join(out, "files", "SHA256SUMS.txt"));
  assert.equal(sums.size, files.length - 1);
  for (const [n, h] of sums) assert.equal(sha(fs.readFileSync(path.join(out, "files", n))), h, n);
  // Notes and pages: the version and the downloads filled in, every picture the README shows copied.
  const notes = fs.readFileSync(path.join(out, "RELEASE_NOTES.md"), "utf8");
  assert.match(notes, new RegExp(`VeloxFetch ${V.replaceAll(".", "\\.")}`));
  assert.match(notes, /\| Windows \| `VeloxFetch_.*_x64-setup\.exe` \|/);
  assert.match(notes, /For Windows, Linux\./);
  for (const f of ["RELEASE_NOTES.md", "LISTINGS.md", path.join("repo", "README.md")]) {
    assert.doesNotMatch(fs.readFileSync(path.join(out, f), "utf8"), /\{\{/, `${f} has no placeholder left`);
  }
  const readme = fs.readFileSync(path.join(out, "repo", "README.md"), "utf8");
  for (const m of readme.matchAll(/\(screenshots\/([a-z0-9-]+\.png)\)/g)) assert.ok(fs.existsSync(path.join(out, "repo", "screenshots", m[1])), m[1]);
  // Staging again replaces an earlier kit.
  stage({ version: V, out, from: [lin] });
  assert.ok(!fs.existsSync(path.join(out, "files", `VeloxFetch_${V}_x64-setup.exe`)));
});

test("staging refuses damaged, unknown, mixed or conflicting files, and never empties a folder it did not make", () => {
  const out = () => path.join(tmp("out"), "kit");
  // A file changed after its checksum was written (damaged on the way from the other computer).
  const damaged = releaseDir(linux());
  fs.appendFileSync(path.join(damaged, `VeloxFetch_${V}_amd64.deb`), "x");
  throwsMsg(() => stage({ version: V, out: out(), from: [damaged] }), /does not match .*SHA256SUMS/);
  // A file the list names but that is not there.
  const missing = releaseDir(linux());
  fs.rmSync(path.join(missing, `VeloxFetch_${V}_amd64.deb`));
  throwsMsg(() => stage({ version: V, out: out(), from: [missing] }), /lists .*which is missing/);
  // A stray artifact nobody expects.
  throwsMsg(() => stage({ version: V, out: out(), from: [releaseDir({ ...linux(), "keylogger.exe": Buffer.from("x") })] }), /unknown name/);
  // An older version left in the folder.
  throwsMsg(() => stage({ version: V, out: out(), from: [releaseDir({ ...linux(), "VeloxFetch_0.1.0_amd64.deb": Buffer.from("old") })] }), /version 0\.1\.0/);
  // The same installer name from two folders with different contents.
  const a = releaseDir(linux());
  const b = releaseDir(linux());
  throwsMsg(() => stage({ version: V, out: out(), from: [a, b] }), /two release folders with different contents/);
  // Only browser extensions: nothing to install.
  throwsMsg(() => stage({ version: V, out: out(), from: [releaseDir({ "VeloxFetch-extension-chromium.zip": Buffer.from("e") })] }), /no installer/);
  // A folder with other things in it is never emptied.
  const busy = tmp("busy");
  fs.writeFileSync(path.join(busy, "thesis.docx"), "precious");
  throwsMsg(() => stage({ version: V, out: busy, from: [releaseDir(linux())] }), /not an earlier publishing kit/);
  assert.equal(fs.readFileSync(path.join(busy, "thesis.docx"), "utf8"), "precious");
});

/** A throwaway project: the files preflight reads, with `keys` as the trusted keys. */
function fakeRoot(keys, version = V) {
  const root = tmp("root");
  fs.mkdirSync(path.join(root, "apps", "desktop", "src-tauri"), { recursive: true });
  fs.writeFileSync(path.join(root, "apps", "desktop", "src-tauri", "tauri.conf.json"), JSON.stringify({ version }));
  fs.writeFileSync(path.join(root, "apps", "desktop", "package.json"), JSON.stringify({ version: V }));
  fs.writeFileSync(path.join(root, "Cargo.toml"), `[workspace.package]\nversion = "${V}"\n`);
  const keysFile = path.join(root, "apps", "desktop", "src-tauri", "update-keys.txt");
  fs.writeFileSync(keysFile, `# trusted keys\n${keys.map((k) => `velox-update-key-v1 ${k}`).join("\n")}\n`);
  return { root, keysFile };
}
const keyFileFor = (pubHex, where) => {
  const id = createHash("sha256").update(Buffer.from(pubHex, "hex")).digest().subarray(0, 8).toString("hex");
  fs.writeFileSync(where, JSON.stringify({ format: "velox-update-secret-key-v1", keyId: id }));
  return where;
};

test("preflight: no release without an update key the app trusts, kept outside the project, and one version", () => {
  const pub = randomBytes(32).toString("hex");
  const keyOutside = keyFileFor(pub, path.join(tmp("key"), "key.json"));
  // No key yet: says how to make one.
  const none = fakeRoot([]);
  throwsMsg(() => preflight({ key: keyOutside, allowDirty: true, ...none }), /no update key yet[\s\S]*update-key\.mjs generate/);
  // A key the app does not trust.
  const other = fakeRoot([randomBytes(32).toString("hex")]);
  throwsMsg(() => preflight({ key: keyOutside, allowDirty: true, ...other }), /is not in update-keys\.txt/);
  // The key file missing, or not a key file.
  const good = fakeRoot([pub]);
  throwsMsg(() => preflight({ key: path.join(good.root, "nope.json"), allowDirty: true, ...good }), /is not there/);
  const junk = path.join(tmp("junk"), "k.json");
  fs.writeFileSync(junk, "{}");
  throwsMsg(() => preflight({ key: junk, allowDirty: true, ...good }), /not an update key file/);
  // Inside the project: refused, whatever it holds.
  throwsMsg(() => preflight({ key: keyFileFor(pub, path.join(good.root, "key.json")), allowDirty: true, ...good }), /never be inside the repository/);
  // Versions that disagree.
  const mixed = fakeRoot([pub], "9.9.9");
  throwsMsg(() => preflight({ key: keyOutside, allowDirty: true, ...mixed }), /one version everywhere/);
  // All good.
  assert.equal(preflight({ key: keyOutside, allowDirty: true, ...good }).version, V);
});

test("the built app must carry the update key and be a release build", () => {
  const pub = randomBytes(32).toString("hex");
  const { keysFile } = fakeRoot([pub]);
  const app = path.join(tmp("app"), "velox-desktop");
  fs.writeFileSync(app, Buffer.concat([randomBytes(512), Buffer.from(`velox-update-key-v1 ${pub}`), randomBytes(512)]));
  checkBinary(app, keysFile);
  fs.writeFileSync(app, randomBytes(1024));
  throwsMsg(() => checkBinary(app, keysFile), /does not carry the update key/);
  fs.writeFileSync(app, Buffer.concat([Buffer.from(pub), Buffer.from("VELOX_DRY_RUN_SHELL")]));
  throwsMsg(() => checkBinary(app, keysFile), /debug build/);
});

const pty = spawnSync("python3", ["-c", "import pty"]).status === 0 && process.platform !== "win32";

test("the whole publish script, in a throwaway copy of the project, with a throwaway key typed at a terminal", { skip: !pty && "needs python3 and a Unix terminal", timeout: 240_000 }, () => {
  const root = tmp("project");
  const copy = (rel) => {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.cpSync(path.join(ROOT, rel), path.join(root, rel), { recursive: true });
  };
  for (const rel of ["scripts/publish-release.sh", "scripts/publish-kit.mjs", "scripts/update-key.mjs", "docs/publish", "apps/desktop/src-tauri/tauri.conf.json", "apps/desktop/package.json", "Cargo.toml"]) copy(rel);
  fs.writeFileSync(path.join(root, "apps/desktop/src-tauri/update-keys.txt"), "# no key yet\n");
  const git = (...a) => execFileSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", ...a], { cwd: root, encoding: "utf8" });
  git("init", "-q");
  fs.writeFileSync(path.join(root, ".gitignore"), "/release/\n/target/\n");
  git("add", "-A");
  git("commit", "-qm", "base");
  const PASS = "a throwaway passphrase for the test";
  const run = (args, limit = 120) => spawnSync("python3", ["-I", path.join(ROOT, "tests/release/pty-run.py"), PASS, String(limit), ...args], { cwd: root, encoding: "utf8" });
  const keyDir = tmp("owner");
  const key = path.join(keyDir, "update-signing-key.json");

  // Without a key the script stops before anything is built, and says how to make one.
  const early = run(["bash", "scripts/publish-release.sh", "--skip-build", "--key", key]);
  assert.notEqual(early.status, 0);
  assert.match(early.stdout, /no update key yet/);

  // The owner makes the key and commits the public half.
  const gen = run(["node", "scripts/update-key.mjs", "generate", "--out", key]);
  assert.equal(gen.status, 0, gen.stdout);
  const pub = fs.readFileSync(path.join(root, "apps/desktop/src-tauri/update-keys.txt"), "utf8").match(/velox-update-key-v1 ([0-9a-f]{64})/)[1];
  // Uncommitted: refused (a public release is built from a commit).
  const dirty = run(["bash", "scripts/publish-release.sh", "--skip-build", "--key", key]);
  assert.match(dirty.stdout, /uncommitted changes/);
  git("commit", "-qam", "update key");

  // A "built" release: the packages and an app carrying the key.
  const rel = path.join(root, "release");
  fs.mkdirSync(rel);
  const files = linux();
  for (const [n, b] of Object.entries(files)) fs.writeFileSync(path.join(rel, n), b);
  fs.writeFileSync(path.join(rel, "SHA256SUMS.txt"), Object.entries(files).map(([n, b]) => `${sha(b)}  ${n}`).join("\n") + "\n");
  fs.mkdirSync(path.join(root, "target/release"), { recursive: true });
  fs.writeFileSync(path.join(root, "target/release/velox-desktop"), Buffer.from(`...velox-update-key-v1 ${pub}...`));
  // A Windows folder brought from the other computer.
  const win = releaseDir({ [`VeloxFetch_${V}_x64-setup.exe`]: randomBytes(4096), "VeloxFetch-portable.zip": randomBytes(1024) });

  const r = run(["bash", "scripts/publish-release.sh", "--skip-build", "--key", key, "--merge", win], 180);
  assert.equal(r.status, 0, r.stdout);
  const kit = path.join(rel, "publish", `v${V}`);
  const out = path.join(kit, "files");
  for (const n of ["latest.json", "latest.json.sig", "SHA256SUMS.txt", `VeloxFetch_${V}_x64-setup.exe`, `VeloxFetch_${V}_amd64.deb`]) assert.ok(fs.existsSync(path.join(out, n)), n);
  // The manifest lists both systems' packages at the public repository, and its signature verifies with the app's key.
  const manifest = JSON.parse(fs.readFileSync(path.join(out, "latest.json"), "utf8"));
  assert.deepEqual(manifest.packages.map((p) => p.target).sort(), ["linux-x86_64-appimage", "linux-x86_64-deb", "linux-x86_64-rpm", "windows-x86_64-nsis"]);
  for (const p of manifest.packages) assert.equal(p.url, `https://github.com/IlyesBs/VeloxFetch/releases/download/v${V}/${p.name}`);
  const verify = spawnSync("node", ["scripts/update-key.mjs", "verify", "--manifest", path.join(out, "latest.json")], { cwd: root, encoding: "utf8" });
  assert.equal(verify.status, 0, verify.stdout + verify.stderr);
  // The checksum list covers every published file, the manifest and its signature included.
  const sums = readSums(path.join(out, "SHA256SUMS.txt"));
  assert.deepEqual([...sums.keys()].sort(), fs.readdirSync(out).filter((n) => n !== "SHA256SUMS.txt").sort());
  // The passphrase was typed, never stored or printed; the private key stayed where it was.
  for (const f of fs.readdirSync(kit, { recursive: true })) {
    const p = path.join(kit, f);
    if (fs.statSync(p).isFile()) assert.ok(!fs.readFileSync(p).includes(PASS) && !fs.readFileSync(p).includes("velox-update-secret-key-v1"), `${f} holds no secret`);
  }
  assert.ok(!r.stdout.includes(PASS), "the passphrase is never echoed");
  assert.match(fs.readFileSync(path.join(kit, "BUILD-INFO.txt"), "utf8"), new RegExp(`VeloxFetch ${V.replaceAll(".", "\\.")}`));
});

test("a system that is not built yet is never promised: macOS lines only with macOS packages", () => {
  const t = "for {{PLATFORMS}} / {{PLATFORMS_FR}} / {{PLATFORMS_AR}} / {{PLATFORMS_LIST}}\nmac line <!-- mac -->\nno-mac line <!-- no-mac -->\nv{{VERSION}}";
  assert.equal(render(t, { version: "1.2.3", systems: ["Windows", "Linux"] }), "for Windows and Linux / Windows et Linux / ويندوز ولينكس / Windows, Linux\nno-mac line\nv1.2.3");
  assert.equal(render(t, { version: "1.2.3", systems: ["Windows", "macOS", "Linux"] }), "for Windows, macOS and Linux / Windows, macOS et Linux / ويندوز وماك ولينكس / Windows, macOS, Linux\nmac line\nv1.2.3");
  // The real texts: without macOS packages, none of them names macOS or leaves a marker or a placeholder.
  for (const name of ["README-public.md", "release-notes.md", "listings.md"]) {
    const text = fs.readFileSync(path.join(ROOT, "docs", "publish", name), "utf8");
    const without = render(text, { version: V, systems: ["Windows", "Linux"] });
    assert.doesNotMatch(without, /macOS|\.dmg|<!-- (no-)?mac -->|\{\{/, name);
    const all = render(text, { version: V, systems: ["Windows", "macOS", "Linux"] });
    assert.doesNotMatch(all, /<!-- (no-)?mac -->|\{\{(?!DOWNLOADS)/, name);
    if (name !== "release-notes.md") assert.match(all, /macOS/, `${name} names macOS once it is built`);
  }
  // A Windows + Linux release: its page says nothing of macOS; the page alone can be made between releases.
  const out = path.join(tmp("out"), "kit");
  stage({ version: V, out, from: [releaseDir(linux()), releaseDir({ [`VeloxFetch_${V}_x64-setup.exe`]: randomBytes(64) })] });
  assert.doesNotMatch(fs.readFileSync(path.join(out, "repo", "README.md"), "utf8"), /macOS/);
  const page = tmp("page");
  writePage(page, { version: V, systems: ["Windows", "Linux"] });
  assert.match(fs.readFileSync(path.join(page, "README.md"), "utf8"), new RegExp(`for \\*\\*Windows and Linux\\*\\*`));
  assert.ok(fs.readdirSync(path.join(page, "screenshots")).length >= 10);
});
