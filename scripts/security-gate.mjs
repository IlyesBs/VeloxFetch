#!/usr/bin/env node
// VeloxFetch security gate (Phase 12): every automated check that must pass before a release, in one
// command that works the same on Windows, macOS and Linux and in CI.
//
//   node scripts/security-gate.mjs            everything (formatting, lint, all tests, audits, SBOM, hashes)
//   node scripts/security-gate.mjs --quick    skip the long test suites (audits + policy + lint only)
//   node scripts/security-gate.mjs --ci       what CI runs in the security job (no test suites: other jobs own them)
//   node scripts/security-gate.mjs --sbom-only
//   node scripts/security-gate.mjs --require-signed   also fail when the installer is not code-signed
//
// Exit code 0 only when every required step passed. A report is written to release/security-gate-report.json.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const args = new Set(process.argv.slice(2));
const win = process.platform === "win32";
const REL = path.join(ROOT, "release");

if (process.env.CARGO_HOME === undefined) {
  const home = process.env.USERPROFILE || process.env.HOME;
  if (home) process.env.PATH = `${process.env.PATH}${path.delimiter}${path.join(home, ".cargo", "bin")}`;
}

const results = [];

function run(name, cmd, cmdArgs, { cwd = ROOT, required = true, hint = "" } = {}) {
  const started = Date.now();
  process.stdout.write(`\n=== ${name}\n    $ ${cmd} ${cmdArgs.join(" ")}\n`);
  const r = spawnSync(cmd, cmdArgs, { cwd, stdio: "inherit", shell: win && /^(npm|npx)$/.test(cmd), env: process.env });
  const ok = r.status === 0;
  const missing = r.error && r.error.code === "ENOENT";
  results.push({ name, ok, required, seconds: Math.round((Date.now() - started) / 1000), note: missing ? `tool not found. ${hint}` : "" });
  return ok;
}

function check(name, fn, { required = true } = {}) {
  process.stdout.write(`\n=== ${name}\n`);
  let note = "";
  let ok = false;
  try {
    const r = fn();
    ok = r === undefined || r === true || (r && r.ok !== false);
    if (r && r.note) note = r.note;
  } catch (e) {
    note = String(e.message ?? e);
    console.log(`    ${note}`);
  }
  results.push({ name, ok, required, seconds: 0, note });
  return ok;
}

function sbom() {
  fs.mkdirSync(path.join(REL, "sbom"), { recursive: true });
  const ok1 = run("SBOM: Rust (CycloneDX 1.5)", "cargo", ["cyclonedx", "--format", "json", "--spec-version", "1.5"], { hint: "cargo install --locked cargo-cyclonedx" });
  // Keep the SBOMs of the programs we ship; drop the per-library ones.
  const shipped = { "velox-desktop": "apps/desktop/src-tauri", "velox-native-host": "apps/native-host", "velox-cli": "apps/cli" };
  const walk = (d) =>
    fs.readdirSync(d, { withFileTypes: true }).flatMap((e) => {
      if (["target", "node_modules", "dist", ".git"].includes(e.name)) return [];
      const p = path.join(d, e.name);
      if (p === REL) return []; // never delete the SBOMs we just collected
      return e.isDirectory() ? walk(p) : e.name.endsWith(".cdx.json") ? [p] : [];
    });
  const collect = () => {
    let n = 0;
    for (const [base, dir] of Object.entries(shipped)) {
      const f = path.join(ROOT, dir, `${base}.cdx.json`);
      if (fs.existsSync(f)) {
        fs.copyFileSync(f, path.join(REL, "sbom", `${base}.cdx.json`));
        n++;
      }
    }
    return n;
  };
  let copied = collect();
  if (copied < Object.keys(shipped).length) {
    // The generator occasionally writes nothing when it runs right after a big build: once more.
    console.log(`    only ${copied} of ${Object.keys(shipped).length} SBOMs were written, running the generator again`);
    run("SBOM: Rust (retry)", "cargo", ["cyclonedx", "--format", "json", "--spec-version", "1.5"], { required: false });
    copied = collect();
  }
  for (const f of walk(ROOT)) fs.rmSync(f); // the per-library files are not shipped
  const ok2 = run("SBOM: UI packages (CycloneDX)", "npm", ["sbom", "--sbom-format", "cyclonedx", "--omit", "dev", "--prefix", "apps/desktop"], { required: false });
  if (ok2) {
    // `npm sbom` prints to stdout; re-run capturing it.
    const r = spawnSync("npm", ["sbom", "--sbom-format", "cyclonedx", "--omit", "dev", "--prefix", "apps/desktop"], { cwd: ROOT, encoding: "utf8", shell: win });
    if (r.status === 0) fs.writeFileSync(path.join(REL, "sbom", "velox-ui.cdx.json"), r.stdout);
  }
  check("SBOM files exist and are valid JSON", () => {
    const files = fs.readdirSync(path.join(REL, "sbom")).filter((f) => f.endsWith(".cdx.json"));
    for (const f of files) {
      const j = JSON.parse(fs.readFileSync(path.join(REL, "sbom", f), "utf8"));
      if (j.bomFormat !== "CycloneDX" || !Array.isArray(j.components) || j.components.length === 0) throw new Error(`${f} is not a usable CycloneDX BOM`);
    }
    if (!files.includes("velox-desktop.cdx.json")) throw new Error("velox-desktop.cdx.json was not produced");
    return { ok: true, note: `${files.length} SBOMs in release/sbom` };
  });
  return ok1;
}

function finish() {
  console.log("\n\n================ SECURITY GATE ================");
  let failed = 0;
  for (const r of results) {
    const mark = r.ok ? "PASS" : r.required ? "FAIL" : "warn";
    if (!r.ok && r.required) failed++;
    console.log(`${mark.padEnd(5)} ${r.name}${r.seconds ? ` (${r.seconds}s)` : ""}${r.note ? `  - ${r.note}` : ""}`);
  }
  fs.mkdirSync(REL, { recursive: true });
  fs.writeFileSync(path.join(REL, "security-gate-report.json"), JSON.stringify({ platform: process.platform, passed: failed === 0, results }, null, 2) + "\n");
  console.log(failed === 0 ? "\nGATE PASSED" : `\nGATE FAILED: ${failed} required step(s) failed`);
  process.exit(failed === 0 ? 0 : 1);
}

if (args.has("--sbom-only")) {
  sbom();
  finish();
}

const full = !args.has("--quick") && !args.has("--ci");

// ---- formatting and lint --------------------------------------------------------------------
if (full || args.has("--quick")) {
  run("Formatting (rustfmt)", "cargo", ["fmt", "--all", "--check"]);
  run("Lint (clippy, warnings are errors)", "cargo", ["clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"]);
}

// ---- tests ----------------------------------------------------------------------------------
if (full) {
  run("Rust tests (unit, integration, robustness)", "cargo", ["test", "--workspace", "--locked"]);
  run("UI tests", "npm", ["test", "--prefix", "apps/desktop"]);
  run("Extension tests", "npm", ["test", "--prefix", "apps/browser-extension"]);
}
run("Security policy tests", process.execPath, ["--test", "tests/security/policy.test.mjs"]);

// ---- dependencies ---------------------------------------------------------------------------
run("Rust advisories (cargo audit)", "cargo", ["audit", "--deny", "warnings"], { hint: "cargo install --locked cargo-audit" });
run("Licences, bans, sources (cargo deny)", "cargo", ["deny", "check"], { hint: "cargo install --locked cargo-deny" });
run("npm advisories: desktop UI", "npm", ["audit", "--prefix", "apps/desktop", "--audit-level=low"]);
run("npm advisories: browser extension", "npm", ["audit", "--prefix", "apps/browser-extension", "--audit-level=low"]);
check("Lockfiles are in sync with the manifests", () => {
  // `--locked` fails when Cargo.lock would have to change; `npm ci --dry-run` fails when package-lock.json is out of date.
  const a = spawnSync("cargo", ["metadata", "--locked", "--format-version", "1"], { cwd: ROOT, encoding: "utf8", maxBuffer: 1 << 28 });
  if (a.status !== 0) throw new Error("Cargo.lock is out of date (cargo metadata --locked failed)");
  for (const dir of ["apps/desktop", "apps/browser-extension"]) {
    const n = spawnSync("npm", ["ci", "--dry-run", "--ignore-scripts", "--prefix", dir], { cwd: ROOT, encoding: "utf8", shell: win });
    if (n.status !== 0) throw new Error(`${dir}/package-lock.json is out of sync with package.json`);
  }
});

// ---- SBOM and artifacts ---------------------------------------------------------------------
if (full || args.has("--ci")) sbom();

check(
  "Release artifacts match SHA256SUMS",
  () => {
    if (!fs.existsSync(path.join(REL, "SHA256SUMS.txt"))) return { ok: true, note: "no release folder yet" };
    const r = spawnSync(process.execPath, ["--test", "tests/security/policy.test.mjs", "--test-name-pattern", "release folder"], { cwd: ROOT, stdio: "inherit" });
    return r.status === 0;
  },
  { required: true },
);

check(
  "Installer is code-signed",
  () => {
    const exe = fs.existsSync(REL) ? fs.readdirSync(REL).find((f) => f.endsWith("-setup.exe")) : undefined;
    if (!exe) return { ok: !args.has("--require-signed"), note: "no installer in release/" };
    if (!win) return { ok: !args.has("--require-signed"), note: "signature check runs on Windows only" };
    const r = spawnSync("powershell", ["-NoProfile", "-Command", `(Get-AuthenticodeSignature '${path.join(REL, exe)}').Status`], { encoding: "utf8" });
    const status = (r.stdout || "").trim();
    return { ok: status === "Valid", note: status === "Valid" ? "signature valid" : `NOT SIGNED (${status || "unknown"}): SmartScreen will warn; see docs/release-process.md` };
  },
  { required: args.has("--require-signed") },
);

finish();
