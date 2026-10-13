#!/usr/bin/env node
// The update signing key and the signed update manifest (Phase 19, docs/auto-update.md).
//
//   node scripts/update-key.mjs generate [--out <file>]       make the key pair (asks a passphrase twice)
//   node scripts/update-key.mjs manifest --version <x.y.z> [--dir <folder>]...   write <first dir>/latest.json
//   node scripts/update-key.mjs sign [--key <file>] [--manifest <file>]   write latest.json.sig (asks the passphrase)
//   node scripts/update-key.mjs verify [--manifest <file>]    check the signature and every listed file
//   (--keys <file>: another trusted-keys file than apps/desktop/src-tauri/update-keys.txt)
//
// The private key is only ever on the owner's computer (better: a removable drive kept offline), encrypted with a
// passphrase (scrypt + AES-256-GCM). It is never read from an environment variable, a command-line argument or a
// pipe: the passphrase is typed at a terminal, without echo. CI never signs. The public key goes into
// apps/desktop/src-tauri/update-keys.txt, which the app compiles in.
//
// No dependencies: Node's own crypto (Ed25519, scrypt, AES-GCM).

import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
let KEYS_FILE = path.join(ROOT, "apps", "desktop", "src-tauri", "update-keys.txt");
const DEFAULT_KEY = path.join(os.homedir(), ".config", "veloxfetch-release", "update-signing-key.json");
// The public download repository (the app's UPDATE_PACKAGES); the source repository is private.
const REPO = "IlyesBs/VeloxFetch-releases";
const PRODUCT = "VeloxFetch";
const SIGNING_CONTEXT = Buffer.from("VeloxFetch update manifest v1\n");
const KEY_PREFIX = "velox-update-key-v1";
const SIG_PREFIX = "velox-update-sig-v1";
const SECRET_FORMAT = "velox-update-secret-key-v1";
const SCRYPT = { N: 1 << 17, r: 8, p: 1, maxmem: 512 * 1024 * 1024 };
const MIN_PASSPHRASE = 16;

/** Release file name -> package target. Only these are ever listed. */
const PACKAGES = [
  [/^VeloxFetch_(\d+\.\d+\.\d+)_x64-setup\.exe$/, "windows-x86_64-nsis"],
  [/^VeloxFetch_(\d+\.\d+\.\d+)_arm64-setup\.exe$/, "windows-aarch64-nsis"],
  [/^VeloxFetch_(\d+\.\d+\.\d+)_amd64\.AppImage$/, "linux-x86_64-appimage"],
  [/^VeloxFetch_(\d+\.\d+\.\d+)_aarch64\.AppImage$/, "linux-aarch64-appimage"],
  [/^VeloxFetch_(\d+\.\d+\.\d+)_amd64\.deb$/, "linux-x86_64-deb"],
  [/^VeloxFetch_(\d+\.\d+\.\d+)_arm64\.deb$/, "linux-aarch64-deb"],
  [/^VeloxFetch-(\d+\.\d+\.\d+)-1\.x86_64\.rpm$/, "linux-x86_64-rpm"],
  [/^VeloxFetch-(\d+\.\d+\.\d+)-1\.aarch64\.rpm$/, "linux-aarch64-rpm"],
  [/^VeloxFetch_(\d+\.\d+\.\d+)_aarch64\.dmg$/, "macos-aarch64-dmg"],
  [/^VeloxFetch_(\d+\.\d+\.\d+)_x64\.dmg$/, "macos-x86_64-dmg"],
];

function fail(msg) {
  console.error(`update-key: ${msg}`);
  process.exit(1);
}

function args(argv) {
  const out = { _: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a.startsWith("--")) {
      const k = a.slice(2);
      const v = argv[i + 1];
      if (v === undefined || v.startsWith("--")) fail(`--${k} needs a value`);
      (out[k] ??= []).push(v);
      i++;
    } else out._.push(a);
  }
  return out;
}

const one = (a, k, d) => (a[k] ? a[k][a[k].length - 1] : d);
const sha256 = (b) => crypto.createHash("sha256").update(b).digest();
const keyId = (pub) => sha256(pub).subarray(0, 8).toString("hex");

function rawPublic(publicKey) {
  // SPKI DER of an Ed25519 key ends with the 32 raw bytes.
  const der = publicKey.export({ format: "der", type: "spki" });
  return Buffer.from(der.subarray(der.length - 32));
}

function hashFile(file) {
  const h = crypto.createHash("sha256");
  const fd = fs.openSync(file, "r");
  try {
    const buf = Buffer.alloc(1 << 20);
    let n;
    while ((n = fs.readSync(fd, buf, 0, buf.length, null)) > 0) h.update(buf.subarray(0, n));
  } finally {
    fs.closeSync(fd);
  }
  return h.digest("hex");
}

/** Ask at the terminal without echo. Never from a pipe. */
function askHidden(prompt) {
  if (!process.stdin.isTTY) fail("the passphrase is only read from a terminal (not a pipe, a file or a variable)");
  return new Promise((resolve) => {
    const stdin = process.stdin;
    // Echo off before the question is shown: what is typed the moment it appears never reaches the screen.
    stdin.setRawMode(true);
    process.stdout.write(prompt);
    stdin.resume();
    let chars = [];
    const done = (value) => {
      stdin.setRawMode(false);
      stdin.pause();
      stdin.removeListener("data", onData);
      process.stdout.write("\n");
      resolve(value);
    };
    const onData = (data) => {
      for (const ch of data.toString("utf8")) {
        if (ch === "\r" || ch === "\n") return done(chars.join(""));
        if (ch === "\u0003") {
          process.stdout.write("\n");
          process.exit(130);
        }
        if (ch === "\u007f" || ch === "\b") chars.pop();
        else chars.push(ch);
      }
    };
    stdin.on("data", onData);
  });
}

function readTrustedKeys() {
  const text = fs.existsSync(KEYS_FILE) ? fs.readFileSync(KEYS_FILE, "utf8") : "";
  const keys = new Map();
  for (const line of text.split(/\r?\n/).map((l) => l.trim()).filter((l) => l && !l.startsWith("#"))) {
    const m = line.match(/^velox-update-key-v1 ([0-9a-f]{64})$/);
    if (!m) fail(`${KEYS_FILE}: not a key line: ${line}`);
    const raw = Buffer.from(m[1], "hex");
    keys.set(keyId(raw), raw);
  }
  return keys;
}

function publicKeyObject(raw) {
  return crypto.createPublicKey({ key: { kty: "OKP", crv: "Ed25519", x: raw.toString("base64url") }, format: "jwk" });
}

function encryptSecret(der, passphrase, id) {
  const salt = crypto.randomBytes(32);
  const nonce = crypto.randomBytes(12);
  const key = crypto.scryptSync(Buffer.from(passphrase, "utf8"), salt, 32, SCRYPT);
  const cipher = crypto.createCipheriv("aes-256-gcm", key, nonce);
  cipher.setAAD(Buffer.from(`${SECRET_FORMAT} ${id}`));
  const ciphertext = Buffer.concat([cipher.update(der), cipher.final()]);
  const tag = cipher.getAuthTag();
  key.fill(0);
  return {
    format: SECRET_FORMAT,
    keyId: id,
    kdf: { name: "scrypt", N: SCRYPT.N, r: SCRYPT.r, p: SCRYPT.p, salt: salt.toString("hex") },
    cipher: { name: "aes-256-gcm", nonce: nonce.toString("hex"), tag: tag.toString("hex") },
    ciphertext: ciphertext.toString("hex"),
  };
}

function decryptSecret(doc, passphrase) {
  if (doc.format !== SECRET_FORMAT || doc.kdf?.name !== "scrypt" || doc.cipher?.name !== "aes-256-gcm") fail("not a VeloxFetch update key file");
  const { N, r, p } = doc.kdf;
  if (N < SCRYPT.N || r < 8 || p < 1) fail("the key file's protection is weaker than allowed");
  const key = crypto.scryptSync(Buffer.from(passphrase, "utf8"), Buffer.from(doc.kdf.salt, "hex"), 32, { N, r, p, maxmem: SCRYPT.maxmem });
  try {
    const d = crypto.createDecipheriv("aes-256-gcm", key, Buffer.from(doc.cipher.nonce, "hex"));
    d.setAAD(Buffer.from(`${SECRET_FORMAT} ${doc.keyId}`));
    d.setAuthTag(Buffer.from(doc.cipher.tag, "hex"));
    return Buffer.concat([d.update(Buffer.from(doc.ciphertext, "hex")), d.final()]);
  } catch {
    fail("wrong passphrase, or the key file was changed");
  } finally {
    key.fill(0);
  }
}

async function generate(a) {
  const out = path.resolve(one(a, "out", DEFAULT_KEY));
  if (out.startsWith(ROOT + path.sep)) fail("the private key must never be inside the project folder");
  if (fs.existsSync(out)) fail(`${out} exists: a key is never overwritten (move it away first)`);
  const p1 = await askHidden(`Passphrase for the new update key (at least ${MIN_PASSPHRASE} characters): `);
  if ([...p1].length < MIN_PASSPHRASE) fail(`the passphrase must have at least ${MIN_PASSPHRASE} characters`);
  const p2 = await askHidden("Same passphrase again: ");
  if (p1 !== p2) fail("the passphrases differ");
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519");
  const pub = rawPublic(publicKey);
  const id = keyId(pub);
  const der = privateKey.export({ format: "der", type: "pkcs8" });
  const doc = encryptSecret(der, p1, id);
  der.fill(0);
  fs.mkdirSync(path.dirname(out), { recursive: true, mode: 0o700 });
  fs.writeFileSync(out, JSON.stringify(doc, null, 2) + "\n", { mode: 0o600, flag: "wx" });
  // Prove the file opens with the passphrase before the public key is trusted.
  const back = crypto.createPrivateKey({ key: decryptSecret(doc, p1), format: "der", type: "pkcs8" });
  if (!rawPublic(crypto.createPublicKey(back)).equals(pub)) fail("the key file does not read back");
  const line = `${KEY_PREFIX} ${pub.toString("hex")}`;
  const existing = fs.existsSync(KEYS_FILE) ? fs.readFileSync(KEYS_FILE, "utf8") : "";
  const stamp = new Date().toISOString().slice(0, 10);
  fs.writeFileSync(KEYS_FILE, `${existing.trim() ? existing.replace(/\s*$/, "\n") : ""}# key ${id}, made ${stamp}\n${line}\n`);
  console.log(`Private key (encrypted): ${out}`);
  console.log(`Key id: ${id}`);
  console.log(`Public key added to ${path.relative(ROOT, KEYS_FILE)}: commit that file.`);
  console.log("Now copy the private key file to two offline drives, then delete it from this computer when you do not sign.");
}

function collect(dirs, version) {
  const found = new Map();
  for (const dir of dirs) {
    const sums = new Map();
    const sumsFile = path.join(dir, "SHA256SUMS.txt");
    if (fs.existsSync(sumsFile)) {
      for (const line of fs.readFileSync(sumsFile, "utf8").split(/\r?\n/).filter(Boolean)) {
        const m = line.match(/^([0-9a-f]{64})\s+\*?(.+)$/);
        if (m) sums.set(m[2], m[1]);
      }
    }
    for (const name of fs.readdirSync(dir)) {
      const rule = PACKAGES.find(([re]) => re.test(name));
      if (!rule) continue;
      const v = name.match(rule[0])[1];
      if (v !== version) fail(`${name} is version ${v}, not ${version}`);
      const file = path.join(dir, name);
      const sha = hashFile(file);
      if (sums.has(name) && sums.get(name) !== sha) fail(`${name} does not match ${sumsFile}`);
      if (found.has(rule[1])) fail(`two packages for ${rule[1]}`);
      found.set(rule[1], { target: rule[1], name, url: `https://github.com/${REPO}/releases/download/v${version}/${name}`, size: fs.statSync(file).size, sha256: sha });
    }
  }
  return [...found.values()].sort((x, y) => x.target.localeCompare(y.target));
}

function manifest(a) {
  const version = one(a, "version");
  if (!/^(0|[1-9]\d{0,5})\.(0|[1-9]\d{0,5})\.(0|[1-9]\d{0,5})$/.test(version ?? "")) fail("--version x.y.z is needed (numbers only)");
  const dirs = (a.dir ?? [path.join(ROOT, "release")]).map((d) => path.resolve(d));
  const packages = collect(dirs, version);
  if (packages.length === 0) fail("no release packages found");
  const doc = { schema: 1, product: PRODUCT, version, released: Math.floor(Date.now() / 1000), packages };
  const out = path.join(dirs[0], "latest.json");
  fs.writeFileSync(out, JSON.stringify(doc, null, 2) + "\n");
  console.log(`${out}: ${packages.length} package(s): ${packages.map((p) => p.target).join(", ")}`);
}

function checkManifest(file, keys) {
  const bytes = fs.readFileSync(file);
  const sig = fs.readFileSync(`${file}.sig`, "utf8").trim().split(/\s+/);
  if (sig.length !== 3 || sig[0] !== SIG_PREFIX) fail("not a VeloxFetch update signature");
  const pub = keys.get(sig[1]);
  if (!pub) fail(`signed with key ${sig[1]}, which ${path.relative(ROOT, KEYS_FILE)} does not trust`);
  const ok = crypto.verify(null, Buffer.concat([SIGNING_CONTEXT, bytes]), publicKeyObject(pub), Buffer.from(sig[2], "hex"));
  if (!ok) fail("the signature does not match the manifest");
  return JSON.parse(bytes.toString("utf8"));
}

async function sign(a) {
  const file = path.resolve(one(a, "manifest", path.join(ROOT, "release", "latest.json")));
  const keyFile = path.resolve(one(a, "key", DEFAULT_KEY));
  const doc = JSON.parse(fs.readFileSync(keyFile, "utf8"));
  const keys = readTrustedKeys();
  if (!keys.has(doc.keyId)) fail(`key ${doc.keyId} is not in ${path.relative(ROOT, KEYS_FILE)}: the app would refuse its signatures`);
  const bytes = fs.readFileSync(file);
  JSON.parse(bytes.toString("utf8"));
  const pass = await askHidden(`Passphrase of update key ${doc.keyId}: `);
  const der = decryptSecret(doc, pass);
  const key = crypto.createPrivateKey({ key: der, format: "der", type: "pkcs8" });
  der.fill(0);
  if (keyId(rawPublic(crypto.createPublicKey(key))) !== doc.keyId) fail("the key file's id does not match its key");
  const sig = crypto.sign(null, Buffer.concat([SIGNING_CONTEXT, bytes]), key);
  fs.writeFileSync(`${file}.sig`, `${SIG_PREFIX} ${doc.keyId} ${sig.toString("hex")}\n`);
  checkManifest(file, keys);
  console.log(`${file}.sig written and verified with ${path.relative(ROOT, KEYS_FILE)}`);
}

function verify(a) {
  const file = path.resolve(one(a, "manifest", path.join(ROOT, "release", "latest.json")));
  const doc = checkManifest(file, readTrustedKeys());
  const dir = path.dirname(file);
  for (const p of doc.packages) {
    const local = path.join(dir, p.name);
    if (!fs.existsSync(local)) {
      console.log(`  ${p.target}: ${p.name} (not in this folder, not re-hashed)`);
      continue;
    }
    if (fs.statSync(local).size !== p.size || hashFile(local) !== p.sha256) fail(`${p.name} does not match the manifest`);
    console.log(`  ${p.target}: ${p.name} ok`);
  }
  console.log(`signature ok: ${doc.product} ${doc.version}`);
}

const a = args(process.argv.slice(2));
const cmd = a._[0];
// Another trusted-keys file (the tests use a throwaway one; a public file, never a secret).
if (a.keys) KEYS_FILE = path.resolve(one(a, "keys"));
const run = { generate, manifest, sign, verify }[cmd];
if (!run) {
  console.log("usage: node scripts/update-key.mjs generate|manifest|sign|verify  (see docs/auto-update.md)");
  process.exit(cmd ? 1 : 0);
}
await run(a);
