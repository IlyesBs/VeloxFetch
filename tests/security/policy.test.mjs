// Phase 12: security policy checks that pin the architecture. They read the repository, not a running
// app, so they run everywhere (CI on every OS) in a second. Run: node --test tests/security/*.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const read = (...p) => fs.readFileSync(path.join(ROOT, ...p), "utf8");
const exists = (...p) => fs.existsSync(path.join(ROOT, ...p));

function walk(dir, out = []) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    if (["target", "node_modules", "dist", ".git", "gen"].includes(e.name)) continue;
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, out);
    else out.push(p);
  }
  return out;
}

/** Rust source of the shipped programs and libraries: no tests, no test-only crates. */
function shippedRust() {
  return [...walk(path.join(ROOT, "crates")), ...walk(path.join(ROOT, "apps"))]
    .filter((f) => f.endsWith(".rs"))
    .filter((f) => !/[\\/](tests|test)[\\/]/.test(f));
}

/** Drop everything from the first `#[cfg(test)]` on: in this code base test modules come last. */
function withoutTestModules(text) {
  const i = text.indexOf("#[cfg(test)]");
  return i < 0 ? text : text.slice(0, i);
}

const lock = () => read("Cargo.lock");
const lockedCrates = () => [...lock().matchAll(/^name = "([^"]+)"$/gm)].map((m) => m[1]);

// ---------------------------------------------------------------------------------------------
// 12.2 Localhost security: there is no local network service to attack

test("no shipped code opens a TCP or UDP listening socket, except the Linux / macOS preview address built to 12.2", () => {
  const bad = [];
  const PREVIEW = path.join("apps", "desktop", "src-tauri", "src", "preview_server.rs");
  const LANES = path.join("crates", "http-client", "src", "lanes.rs");
  for (const f of shippedRust()) {
    const text = withoutTestModules(fs.readFileSync(f, "utf8"));
    const rel = path.relative(ROOT, f);
    if (/\b(TcpListener|UdpSocket)\b|Type::(DGRAM|STREAM|RAW)/.test(text) && rel !== PREVIEW && rel !== LANES) bad.push(rel);
  }
  // Several networks at once asks the system which route it would use to a server (so a VPN is never gone around):
  // one datagram socket, never bound by hand, never sent on, read or listening, closed at once. Nothing else.
  const lanes = withoutTestModules(read("crates", "http-client", "src", "lanes.rs"));
  assert.equal((lanes.match(/Type::DGRAM/g) ?? []).length, 1, "one route question");
  assert.ok(!/TcpListener|UdpSocket|Type::(STREAM|RAW)|\.bind\(|\.listen\(|\.send|\.recv|\.accept\(/.test(lanes), "never bound, sent on, read or listening");
  const ask = lanes.slice(lanes.indexOf("pub fn system_source_for"), lanes.indexOf("pub fn spreadable"));
  assert.match(ask, /s\.connect\(&to\.into\(\)\)\.ok\(\)\?;\s*s\.local_addr\(\)/, "connect (no packet), read the chosen address, done");
  assert.deepEqual(bad, [], "a local HTTP/TCP API needs the 12.2 design (127.0.0.1 + token + Origin + schemas + rate limits) first");
  // The one exception (WebKitGTK's player cannot read a custom protocol; WKWebView's cannot stream a growing file
  // through one): 12.2 in full, Linux and macOS only (Windows keeps the custom protocol), read-only.
  const pv = withoutTestModules(read("apps", "desktop", "src-tauri", "src", "preview_server.rs"));
  const lb = pv.slice(pv.indexOf("pub mod loopback"));
  assert.match(pv, /#\[cfg\(any\(target_os = "linux", target_os = "macos"\)\)\]\s*pub mod loopback/, "Linux and macOS only");
  assert.ok(!/UdpSocket/.test(lb) && (lb.match(/bind\(/g) ?? []).length === 1, "one listener");
  assert.match(lb, /TcpListener::bind\(\("127\.0\.0\.1", 0\)\)/, "this computer only, a free port");
  assert.match(lb, /read_exact\(&mut b\)/, "a random secret from the system (32 bytes)");
  assert.match(lb, /strip_prefix\(secret\)\)\.ok_or\(StatusCode::NOT_FOUND\)/, "nothing without the secret");
  assert.match(lb, /"host" => host_ok = v\.trim\(\) == host/, "Host checked (DNS rebinding)");
  assert.match(lb, /"origin" => origin_ok = v\.trim\(\) == APP_ORIGIN/, "Origin checked");
  assert.match(lb, /"GET" => false,\s*"HEAD" => true,\s*_ => return Err\(StatusCode::METHOD_NOT_ALLOWED\)/, "read-only methods");
  assert.match(lb, /target_from_path\(rest\)/, "ids only, the same schema as the custom protocol");
  assert.match(lb, /const MAX_HEAD: usize = /, "request size limit");
  assert.match(lb, /const MAX_OPEN: usize = \d+;\s*const MAX_PER_SECOND: u32 = \d+;/, "rate limits");
  assert.match(lb, /StatusCode::TOO_MANY_REQUESTS/);
  assert.ok(!/access-control-allow-origin/i.test(pv), "no CORS at all");
  assert.match(lb, /manager\.read_preview\(id, Some\(&\w+\)\)/, "the same reader: only written audio / video bytes");
  // macOS: plain http only for local addresses (App Transport Security stays on for everything else).
  const plist = read("apps", "desktop", "src-tauri", "packaging", "macos", "Info.plist");
  assert.match(plist, /<key>NSAllowsLocalNetworking<\/key>\s*<true\/>/);
  assert.doesNotMatch(plist, /NSAllowsArbitraryLoads|NSExceptionDomains/, "no wider exception");
});

test("the browser link is a named pipe / unix socket, never a TCP port", () => {
  const transport = read("crates", "browser-bridge", "src", "transport.rs");
  assert.match(transport, /pipe|UnixListener|UnixStream/i);
  assert.doesNotMatch(withoutTestModules(transport), /TcpListener|TcpStream|SocketAddr/);
});

// ---------------------------------------------------------------------------------------------
// 12.1 IPC / WebView

const conf = () => JSON.parse(read("apps", "desktop", "src-tauri", "tauri.conf.json"));

test("the webview CSP is strict: same origin only, no eval, no inline script, no remote origins", () => {
  const csp = conf().app.security.csp;
  assert.equal(csp["default-src"], "'self'");
  for (const [directive, value] of Object.entries(csp)) {
    assert.doesNotMatch(value, /unsafe-eval|unsafe-inline|\*|https?:\/\/(?!ipc\.localhost|velox-preview\.localhost)|data:/, `${directive}: ${value}`);
  }
  // Video / audio only from the app's own preview protocol (served by preview_server.rs, by download id).
  assert.equal(csp["media-src"], "velox-preview: http://velox-preview.localhost");
  assert.doesNotMatch(csp["script-src"], /unsafe/);
  assert.equal(csp["connect-src"], "ipc: http://ipc.localhost", "the UI may talk to the Rust core and nothing else");
  assert.ok(!("frame-src" in csp) || csp["frame-src"] === "'none'");
});

test("the webview is locked down: no global Tauri object, frozen prototypes, no remote IPC", () => {
  const c = conf();
  assert.equal(c.app.withGlobalTauri, false);
  assert.equal(c.app.security.freezePrototype, true);
  const text = read("apps", "desktop", "src-tauri", "tauri.conf.json") + read("apps", "desktop", "src-tauri", "capabilities", "main-window.json");
  assert.doesNotMatch(text, /dangerousRemoteDomainIpcAccess|dangerousDisableAssetCspModification|"remote"|assetProtocol/);
});

test("capabilities name explicit app commands only: no default set, no plugin permissions", () => {
  const files = fs.readdirSync(path.join(ROOT, "apps", "desktop", "src-tauri", "capabilities")).filter((f) => f.endsWith(".json"));
  assert.ok(files.length >= 1);
  for (const f of files) {
    const cap = JSON.parse(read("apps", "desktop", "src-tauri", "capabilities", f));
    assert.ok(Array.isArray(cap.windows) && cap.windows.every((w) => !w.includes("*")), `${f}: windows must be named, not wildcards`);
    assert.ok(!cap.remote, `${f}: no remote origins`);
    for (const p of cap.permissions) {
      const name = typeof p === "string" ? p : p.identifier;
      // Our own commands, plus the few Tauri core permissions a window needs: subscribe to app events,
      // show / focus itself (the window is created hidden and shown once painted), and the controls of the UI's own
      // title bar (drag it, double-click to maximize; its buttons use our own `window_control`). Nothing that creates,
      // closes or reaches other windows, and nothing that reaches files.
      const CORE = [
        "core:event:allow-listen",
        "core:event:allow-unlisten",
        "core:window:allow-show",
        "core:window:allow-set-focus",
        "core:window:allow-start-dragging",
        "core:window:allow-internal-toggle-maximize",
      ];
      const ok = /^allow-[a-z0-9-]+$/.test(name) || CORE.includes(name);
      assert.ok(ok, `${f}: ${name} is not one of our own allow-<command> permissions`);
      assert.doesNotMatch(name, /^(fs|shell|http|opener|dialog|updater)[:-]|:default$/);
    }
  }
});

test("the UI cannot open windows, run programs, read files or reach the network by itself", () => {
  const bad = [];
  for (const f of walk(path.join(ROOT, "apps", "desktop", "src")).filter((f) => /\.(ts|tsx)$/.test(f) && !/\.test\./.test(f))) {
    const text = fs.readFileSync(f, "utf8");
    if (/@tauri-apps\/plugin-|\bfetch\(|XMLHttpRequest|WebSocket|dangerouslySetInnerHTML|\beval\(|new Function\(|innerHTML\s*=|window\.open\(|document\.write/.test(text)) {
      bad.push(path.relative(ROOT, f));
    }
  }
  assert.deepEqual(bad, []);
  // The only Tauri package the UI depends on is the core IPC bridge.
  const pkg = JSON.parse(read("apps", "desktop", "package.json"));
  const tauriDeps = Object.keys({ ...pkg.dependencies, ...pkg.devDependencies }).filter((d) => d.startsWith("@tauri-apps/"));
  for (const d of tauriDeps) assert.ok(["@tauri-apps/api", "@tauri-apps/cli"].includes(d), `unexpected ${d}`);
});

// ---------------------------------------------------------------------------------------------
// 12.3 Supply chain: lockfiles, sources, banned crates

test("dependencies are pinned by lockfiles that are present", () => {
  for (const f of [["Cargo.lock"], ["apps", "desktop", "package-lock.json"], ["apps", "browser-extension", "package-lock.json"]]) {
    assert.ok(exists(...f), `${f.join("/")} is missing`);
  }
  assert.ok(exists("deny.toml"));
});

test("every Rust dependency comes from crates.io and has a checksum (no git, no path-outside-repo)", () => {
  const packages = lock().split("[[package]]").slice(1);
  for (const p of packages) {
    const name = p.match(/name = "([^"]+)"/)[1];
    const source = p.match(/source = "([^"]+)"/)?.[1];
    if (!source) continue; // a workspace crate
    assert.equal(source, "registry+https://github.com/rust-lang/crates.io-index", `${name} comes from ${source}`);
    assert.match(p, /checksum = "[0-9a-f]{64}"/, `${name} has no checksum`);
  }
});

test("npm lockfiles pin every package by integrity hash from the npm registry over https", () => {
  for (const f of [["apps", "desktop", "package-lock.json"], ["apps", "browser-extension", "package-lock.json"]]) {
    const lockJson = JSON.parse(read(...f));
    for (const [name, p] of Object.entries(lockJson.packages ?? {})) {
      if (name === "" || p.link) continue;
      assert.ok(p.integrity, `${f.join("/")}: ${name} has no integrity hash`);
      assert.ok(!p.resolved || p.resolved.startsWith("https://registry.npmjs.org/"), `${name} resolves to ${p.resolved}`);
    }
  }
});

test("the browser extension has no dependencies at all", () => {
  const pkg = JSON.parse(read("apps", "browser-extension", "package.json"));
  assert.deepEqual(Object.keys(pkg.dependencies ?? {}), []);
  assert.deepEqual(Object.keys(pkg.devDependencies ?? {}), []);
});

test("banned crates are not in the dependency graph", () => {
  const banned = ["openssl", "openssl-sys", "native-tls", "tauri-plugin-shell", "tauri-plugin-fs", "tauri-plugin-http", "tauri-plugin-opener", "tauri-plugin-updater"];
  const present = new Set(lockedCrates());
  for (const b of banned) assert.ok(!present.has(b), `${b} must not be a dependency (see deny.toml)`);
  assert.ok(present.has("rustls"), "TLS is rustls");
});

test("cargo-deny is configured to reject unknown registries, git sources, wildcards and unreviewed licences", () => {
  const deny = read("deny.toml");
  assert.match(deny, /unknown-registry\s*=\s*"deny"/);
  assert.match(deny, /unknown-git\s*=\s*"deny"/);
  assert.match(deny, /wildcards\s*=\s*"deny"/);
  assert.match(deny, /yanked\s*=\s*"deny"/);
  assert.doesNotMatch(deny, /"(GPL|AGPL|LGPL)[^"]*"\s*,/, "no copyleft licence may be allowed");
  // Every ignored advisory must say why.
  for (const m of deny.matchAll(/\{\s*id\s*=\s*"RUSTSEC-[^}]*\}/g)) assert.match(m[0], /reason\s*=\s*".{15,}"/, m[0]);
});

// ---------------------------------------------------------------------------------------------
// 12.4 Build pipeline and release integrity

test("CI runs every security step, from a locked dependency set, on all three operating systems", () => {
  assert.ok(exists(".github", "workflows", "ci.yml"), "the test workflow is missing");
  assert.ok(exists(".github", "workflows", "build.yml"), "the build workflow is missing");
  const ci = read(".github", "workflows", "ci.yml");
  const build = read(".github", "workflows", "build.yml");
  for (const step of ["cargo fmt", "cargo clippy", "cargo test", "cargo audit", "cargo deny", "npm audit", "npm ci", "security-gate", "sbom"]) {
    assert.match(ci, new RegExp(step, "i"), `the tests have no "${step}" step`);
  }
  for (const os of ["windows-latest", "macos-latest", "ubuntu-latest"]) assert.match(ci, new RegExp(os), `tests on ${os}`);
  for (const os of ["windows-latest", "macos-latest", "ubuntu-22.04"]) assert.match(build, new RegExp(os), `build on ${os}`);
  assert.match(build, /scripts\/build-release\.sh/, "CI builds exactly like a local build (locked installs, SHA256SUMS)");
  // Both workflows run only when started by hand ("Run workflow" -> pick the branch); nothing runs on its own.
  for (const [name, wf] of [["ci", ci], ["build", build]]) {
    assert.match(wf, /^on:\s*\n\s*workflow_dispatch:/m, `${name} runs only when started by hand`);
    assert.doesNotMatch(wf, /^\s*push:\s*$/m, `${name} must not run automatically on push`);
    assert.doesNotMatch(wf, /^\s*pull_request:\s*$/m, `${name} must not run automatically on a pull request`);
  }
  assert.match(build, /sha256sum/, "the published files are hashed");
  assert.match(build, /--draft/, "a release is never published unsigned");
  assert.match(ci, /--locked/, "builds must not update the lockfile");
  assert.match(read("scripts", "build-release.sh"), /cargo build --release --locked/);
  for (const wf of [ci, build]) {
    assert.doesNotMatch(wf, /pull_request_target/, "untrusted pull requests must never run with secrets");
    assert.match(wf, /permissions:\s*\n\s*contents:\s*read/, "the workflow token is read-only by default");
    // Third-party actions are pinned to a full commit hash, not a movable tag.
    for (const m of wf.matchAll(/uses:\s*([^\s@]+)@([^\s#]+)/g)) {
      assert.match(m[2], /^[0-9a-f]{40}$/, `${m[1]}@${m[2]} is not pinned to a commit`);
    }
  }
});

test("updates are only our own signed design (Phase 19, docs/auto-update.md)", () => {
  const cargo = read("apps", "desktop", "src-tauri", "Cargo.toml");
  // Not Tauri's updater plugin (banned in deny.toml): our own, reviewed one.
  assert.doesNotMatch(cargo, /tauri-plugin-updater/);
  assert.doesNotMatch(read("apps", "desktop", "src-tauri", "tauri.conf.json"), /updater|pubkey|endpoints/i);
  // The trusted keys: only well-formed public keys, never anything else.
  for (const line of read("apps", "desktop", "src-tauri", "update-keys.txt").split(/\r?\n/).map((l) => l.trim())) {
    if (!line || line.startsWith("#")) continue;
    assert.match(line, /^velox-update-key-v1 [0-9a-f]{64}$/, `not a public key line: ${line}`);
  }
  // A signature is checked strictly, over a context-prefixed manifest, before the manifest is read.
  const verify = read("crates", "update-verify", "src", "lib.rs");
  assert.match(verify, /verify_strict\(/);
  assert.match(verify, /SIGNING_CONTEXT/);
  assert.ok(verify.indexOf("verify_strict(") < verify.indexOf("serde_json::from_slice(manifest)"), "the signature is checked before the manifest is parsed");
  // A downloaded package keeps its final name only after its size and hash matched, and is checked again before use.
  const mgr = read("crates", "download-manager", "src", "manager", "update.rs");
  const fetch = mgr.slice(mgr.indexOf("async fn fetch_package"));
  assert.ok(fetch.indexOf("check_file(") < fetch.indexOf("std::fs::rename(&part, &final_path)"), "verified before it is renamed");
  assert.match(mgr.slice(mgr.indexOf("pub async fn prepare_update_install")), /check_file\(/);
  const inst = read("crates", "system-integration", "src", "update.rs");
  assert.match(inst, /share_mode\(FILE_SHARE_READ\)/, "Windows: the setup cannot be swapped between the check and the start");
  const setup = inst.slice(inst.indexOf("fn run_windows_setup("));
  assert.ok(setup.indexOf("verify(") > 0 && setup.indexOf("verify(") < setup.indexOf("ShellExecuteExW(&mut"), "the setup is checked before it is started");
  const install = inst.slice(inst.indexOf("pub fn install("), inst.indexOf("pub fn replace_file("));
  assert.ok(install.indexOf("verify(") < install.indexOf("open_with_system("), "a package is checked before it is opened");
  // Only a constant https page is ever opened in the browser.
  assert.match(inst.slice(inst.indexOf("pub fn open_release_page(")), /starts_with\("https:\/\/"\)/);
  // The private key never enters the repository, and the passphrase never comes from a variable or an argument.
  const tool = read("scripts", "update-key.mjs");
  assert.doesNotMatch(tool, /process\.env/);
  assert.match(tool, /isTTY/);
  for (const f of walk(ROOT)) {
    // The tools that read key files name their format; a key file is anything else that holds it.
    if (/update-key\.mjs$|publish-kit(\.test)?\.mjs$|\.md$|policy\.test\.mjs$/.test(f)) continue;
    const text = fs.readFileSync(f).toString("latin1");
    assert.doesNotMatch(text, /velox-update-secret-key-v1/, `${path.relative(ROOT, f)} looks like a private update key`);
  }
  // Updates come from the fixed HTTPS release address in the build, never from the UI.
  const lib = read("apps", "desktop", "src-tauri", "src", "lib.rs");
  // The repository whose releases users download: the app, the manifest tool and the publish script all name the
  // same one, or the manifest would list packages the app refuses.
  const PUBLIC = "IlyesBs/VeloxFetch";
  // The update addresses are generated into identity.rs from config/veloxfetch.yaml (config.test.mjs checks they
  // are in step); they are still what the built binary trusts, so the invariant is asserted on the compiled values.
  const ids = read("apps", "desktop", "src-tauri", "src", "generated", "identity.rs");
  assert.match(ids, /const UPDATE_FEED: &str = "https:\/\/github\.com\/IlyesBs\/VeloxFetch\/releases\/latest\/download\/latest\.json"/);
  assert.match(ids, /const UPDATE_PACKAGES: &str = "https:\/\/github\.com\/IlyesBs\/VeloxFetch\/releases\/download\/"/);
  assert.match(ids, /const RELEASE_PAGE: &str = "https:\/\/github\.com\/IlyesBs\/VeloxFetch\/releases"/);
  // The newest release is watched (announced only): GitHub's fixed address in this repository, https.
  assert.match(ids, /const LATEST_RELEASE: &str = "https:\/\/github\.com\/IlyesBs\/VeloxFetch\/releases\/latest"/);
  const upd = withoutTestModules(read("crates", "download-manager", "src", "manager", "update.rs"));
  const watch = upd.slice(upd.indexOf("async fn watch_once"), upd.indexOf("async fn check_once"));
  assert.match(watch, /released_version\(&cfg\.release_page, resp\.url\(\)\.as_str\(\)\)/, "only a tag of this release page counts");
  assert.match(watch, /Choice::NoPackage\(version\)/, "a watched version is only announced (the release page)");
  assert.ok(!/fetch_small|download|Package/.test(watch.replace("Choice::NoPackage", "")), "nothing is downloaded from a watched release");
  assert.match(tool, new RegExp(`const REPO = "${PUBLIC}"`));
  assert.match(read("scripts", "publish-release.sh"), new RegExp(`^PUBLIC_REPO="${PUBLIC}"$`, "m"));
  assert.doesNotMatch(lib, /fn (download_update|install_update|check_for_updates)\([^)]*(url|path|String)/);
});

test("the publishing kit signs only from a terminal, publishes only drafts, and ships only checked files (docs/publishing.md)", () => {
  const sh = read("scripts", "publish-release.sh");
  const kit = read("scripts", "publish-kit.mjs");
  assert.match(sh, /^set -euo pipefail$/m);
  assert.match(sh, /\[ -t 0 \] \|\| die/, "the passphrase is typed at a terminal, never piped");
  // The only signing is update-key.mjs's, which asks the passphrase itself; nothing hands it one.
  assert.doesNotMatch(sh, /--passphrase|--pass\b|<<<|\| *node scripts\/update-key|PASSPHRASE=/);
  // GitHub: a draft or nothing; never published, edited or deleted by the script.
  for (const m of sh.matchAll(/gh release (\w+)/g)) assert.equal(m[1], "create");
  assert.match(sh, /gh release create [^\n]*--draft/);
  assert.doesNotMatch(sh, /gh release (edit|delete|upload)|--draft=false|git push|git tag/);
  // The full security gate runs unless the owner skips the build entirely.
  assert.doesNotMatch(sh, /build-release\.sh[^\n]*--(skip-gate|quick-gate)/);
  // The kit reads no secret from the environment and refuses unknown or unchecked files.
  assert.doesNotMatch(kit, /process\.env/);
  assert.match(kit, /not a file a release ships/);
  assert.match(kit, /does not match .*damaged, or changed after the build/);
  assert.match(kit, /must never be inside the repository/);
  assert.match(read(".gitattributes"), /^\*\.sh text eol=lf$/m);
});

test("the release folder, when present, matches SHA256SUMS exactly", () => {
  const rel = path.join(ROOT, "release");
  if (!fs.existsSync(path.join(rel, "SHA256SUMS.txt"))) return; // a fresh checkout has no release yet
  const listed = new Map();
  for (const line of fs.readFileSync(path.join(rel, "SHA256SUMS.txt"), "utf8").split(/\r?\n/).filter(Boolean)) {
    const m = line.match(/^([0-9a-f]{64})\s+\*?(.+)$/);
    assert.ok(m, `bad line: ${line}`);
    listed.set(m[2], m[1]);
  }
  assert.ok(listed.size >= 1);
  for (const [name, hash] of listed) {
    const file = path.join(rel, name);
    assert.ok(fs.existsSync(file), `${name} is listed but missing`);
    assert.equal(createHash("sha256").update(fs.readFileSync(file)).digest("hex"), hash, `${name} does not match its checksum`);
  }
  const shipped = fs.readdirSync(rel).filter((n) => /\.(exe|zip|msi|dmg|deb|rpm|AppImage)$/.test(n));
  for (const n of shipped) assert.ok(listed.has(n), `${n} is shipped without a checksum`);
});

// ---------------------------------------------------------------------------------------------
// Process hardening and untrusted-code paths

test("every shipped program restricts DLL loading first thing in main", () => {
  for (const f of [["apps", "desktop", "src-tauri", "src", "main.rs"], ["apps", "native-host", "src", "main.rs"], ["apps", "cli", "src", "main.rs"]]) {
    const text = read(...f);
    const main = text.slice(text.indexOf("fn main()"));
    assert.match(main.split("\n").slice(0, 4).join("\n"), /harden_process\(\)/, `${f.join("/")} must call path_safety::harden_process() before anything else`);
  }
});

test("external programs are started with an argument list, never through a shell", () => {
  const bad = [];
  for (const f of shippedRust()) {
    const text = withoutTestModules(fs.readFileSync(f, "utf8"));
    if (/Command::new\(\s*"(sh|bash|zsh|cmd|cmd\.exe|powershell|powershell\.exe|pwsh)"/i.test(text)) bad.push(`${path.relative(ROOT, f)}: shell program`);
    if (/\.arg\(\s*"(-c|\/[Cc])"\s*\)|\.args\(\s*\[\s*"(-c|\/[Cc])"/.test(text)) bad.push(`${path.relative(ROOT, f)}: shell command string`);
  }
  assert.deepEqual(bad, []);
});

test("secrets never reach logs through format strings of cookies, tokens or credentials", () => {
  const bad = [];
  for (const f of shippedRust()) {
    const text = withoutTestModules(fs.readFileSync(f, "utf8"));
    for (const m of text.matchAll(/(tracing::(?:trace|debug|info|warn|error)!|println!|eprintln!|format!)\(([^;]{0,240})\)/g)) {
      if (/\b(cookies?|password|authorization|request_headers|token|secret)\b\s*[=,)]/i.test(m[2]) && !/redact|\*\*\*|hidden|"[^"]*token file|Token\(\.\.\)/i.test(m[2])) {
        bad.push(`${path.relative(ROOT, f)}: ${m[0].slice(0, 100)}`);
      }
    }
  }
  assert.deepEqual(bad, []);
});

// ---------------------------------------------------------------------------------------------
// Phase 13: the crash-test hook and the "never opens archives" promise

test("the crash hook is compiled in only by the crash tests, never by a shipped program", () => {
  const own = read("crates", "failpoint", "Cargo.toml");
  assert.match(own, /^\[features\][\s\S]*^enabled\s*=\s*\[/m, "the crate has an `enabled` feature");
  assert.doesNotMatch(own, /^default\s*=/m, "`enabled` must never be a default feature");
  const offenders = [];
  for (const f of walk(ROOT).filter((x) => /[\/]Cargo\.toml$/.test(x) && !x.includes(`${path.sep}target${path.sep}`))) {
    const rel = path.relative(ROOT, f).split(path.sep).join("/");
    const text = fs.readFileSync(f, "utf8");
    if (!/failpoint/.test(text) || rel === "crates/failpoint/Cargo.toml") continue;
    const [normal, ...rest] = text.split(/^\[dev-dependencies\]/m);
    // Outside [dev-dependencies] a program may depend on the crate, but never switch it on.
    if (/failpoint[^\n]*enabled/.test(normal)) offenders.push(`${rel}: enables failpoint outside [dev-dependencies]`);
    if (rel !== "tests/integration/Cargo.toml" && /failpoint[^\n]*enabled/.test(rest.join(""))) offenders.push(`${rel}: only tests/integration may switch it on`);
  }
  assert.deepEqual(offenders, []);
  const desktop = read("apps", "desktop", "src-tauri", "Cargo.toml") + read("apps", "cli", "Cargo.toml") + read("apps", "native-host", "Cargo.toml");
  assert.doesNotMatch(desktop, /failpoint/, "no shipped program depends on the crash hook directly");
});

test("every crash point in the code is exercised by a crash test", () => {
  const points = new Set();
  for (const f of shippedRust().filter((x) => !x.includes(`${path.sep}failpoint${path.sep}`))) {
    for (const m of fs.readFileSync(f, "utf8").matchAll(/failpoint::hit(?:_io)?\("([^"]+)"\)/g)) points.add(m[1]);
  }
  assert.ok(points.size >= 20, `expected the crash points to exist, found ${[...points]}`);
  const crash = read("tests", "integration", "tests", "crash.rs");
  const untested = [...points].filter((p) => !crash.includes(`"${p}`) && !crash.includes(`${p}@`) && !crash.includes(`"${p}"`));
  assert.deepEqual(untested, [], "a crash point nobody kills the process at is dead weight");
});

test("no archive library is linked: downloaded archives are saved, never opened", () => {
  const banned = ["zip", "tar", "unrar", "sevenz-rust", "sevenz-rust2", "async_zip", "async-tar", "zip-extract", "tokio-tar", "bzip2", "xz2", "lzma-rs", "unarc-rs"];
  const present = lockedCrates().filter((c) => banned.includes(c));
  assert.deepEqual(present, [], "an extractor needs its own path-traversal / zip-bomb design first (docs/threat-model.md, threat 4)");
});

test("the Phase 13 test suites exist", () => {
  for (const f of ["crash.rs", "servers.rs", "hostile.rs", "state_machine.rs", "fs_security.rs", "secrets.rs"]) {
    assert.ok(exists("tests", "integration", "tests", f), `tests/integration/tests/${f}`);
  }
});

test("built programs contain no crash hook (checked in the binaries themselves, when they are built)", () => {
  const checked = [];
  for (const profile of ["release", "debug"]) {
    for (const name of ["velox-desktop", "velox-native-host", "velox-cli"]) {
      for (const ext of ["", ".exe"]) {
        const f = path.join(ROOT, "target", profile, name + ext);
        if (!fs.existsSync(f) || !fs.statSync(f).isFile()) continue;
        checked.push(path.relative(ROOT, f));
        assert.ok(!fs.readFileSync(f).includes("VELOX_FAILPOINT"), `${f} was built with the crash hook enabled`);
      }
    }
  }
  console.log(`    (looked at ${checked.length} built program(s): ${checked.join(", ") || "none built"})`);
});

/** The DLL names a Windows program imports (PE import directory). */
function peImports(buf) {
  const pe = buf.readUInt32LE(0x3c);
  const nsec = buf.readUInt16LE(pe + 6);
  const opt = pe + 24;
  const dd = opt + (buf.readUInt16LE(opt) === 0x20b ? 112 : 96);
  const secs = [];
  for (let i = 0; i < nsec; i++) {
    const s = opt + buf.readUInt16LE(pe + 20) + i * 40;
    secs.push({ va: buf.readUInt32LE(s + 12), size: Math.max(buf.readUInt32LE(s + 8), buf.readUInt32LE(s + 16)), raw: buf.readUInt32LE(s + 20) });
  }
  const off = (rva) => {
    const s = secs.find((x) => rva >= x.va && rva < x.va + x.size);
    if (!s) throw new Error(`bad rva ${rva}`);
    return rva - s.va + s.raw;
  };
  const out = [];
  for (let p = off(buf.readUInt32LE(dd + 8)); buf.readUInt32LE(p + 12) !== 0; p += 20) {
    const o = off(buf.readUInt32LE(p + 12));
    out.push(buf.toString("latin1", o, buf.indexOf(0, o)).toLowerCase());
  }
  return out;
}

test("shipped Windows programs do not need the Visual C++ runtime installed (checked in the release binaries, when built)", () => {
  // A browser starts velox-native-host.exe on its own: on a computer without the "Visual C++ Redistributable" a program that
  // imports VCRUNTIME140.dll cannot start at all, and the extension only sees a broken link.
  const checked = [];
  for (const name of ["velox-desktop.exe", "velox-native-host.exe"]) {
    const f = path.join(ROOT, "target", "release", name);
    if (!fs.existsSync(f)) continue;
    checked.push(name);
    const dlls = peImports(fs.readFileSync(f));
    assert.ok(dlls.includes("kernel32.dll"), `could not read the imports of ${name}`);
    for (const dll of dlls) assert.ok(!/^(vcruntime|msvcp)\d+(_\d+)?\.dll$/.test(dll), `${name} needs ${dll}, which a fresh Windows does not have`);
  }
  assert.ok(fs.readFileSync(path.join(ROOT, "apps", "native-host", "build.rs"), "utf8").includes("/DEFAULTLIB:{lib}.lib"), "the host links the runtime statically");
  console.log(`    (looked at ${checked.length} release program(s): ${checked.join(", ") || "none built"})`);
});

// ---------------------------------------------------------------------------------------------
// Download folders: the web view never names a path

test("no command takes a path from the web view; folders are chosen in a native dialog and referred to by token", () => {
  const src = read("apps", "desktop", "src-tauri", "src", "lib.rs");
  // Argument lists of `command!(...)` and `#[tauri::command]` functions.
  const args = [...src.matchAll(/command!\(\s*\w+,\s*\w+,\s*\(([^)]*)\)/g)].map((m) => m[1]);
  args.push(...[...src.matchAll(/#\[tauri::command\][\s\S]*?async fn \w+\(([\s\S]*?)\)\s*->/g)].map((m) => m[1]));
  assert.ok(args.length > 30, "found the command signatures");
  const bad = args.filter((a) => /\b(path|dir|folder|directory|file)\s*:\s*(String|&str|PathBuf|&Path|Option<String>)/.test(a) || /\bPathBuf\b|&Path\b/.test(a));
  assert.deepEqual(bad, [], "a command that takes a path from the UI defeats the folder rules (docs/threat-model.md, threat 16)");
  // Requests deserialised from the UI carry no path either.
  const api = read("crates", "download-manager", "src", "api.rs");
  for (const m of api.matchAll(/#\[derive\([^)]*Deserialize[^)]*\)\][^{]*\{([^}]*)\}/g)) {
    assert.doesNotMatch(m[1], /PathBuf|Path\b/, `a request type has a path field: ${m[0].slice(0, 80)}`);
  }
  // The dialog is ours, not a plugin the web view could drive.
  const conf = read("apps", "desktop", "src-tauri", "Cargo.toml");
  assert.match(conf, /^rfd\s*=/m);
  assert.doesNotMatch(conf, /tauri-plugin-dialog/);
  assert.ok(!lockedCrates().includes("tauri-plugin-dialog"));
});

test("engines write only under folders the manager allows, checked again before every write", () => {
  const engine = read("crates", "downloader-core", "src", "engine.rs");
  assert.match(engine, /fn confine\(&self/);
  assert.match(engine, /roots\.check\(path\)/);
  const mgr = read("crates", "download-manager", "src", "manager.rs");
  assert.match(mgr, /\.with_sandbox\(roots\.clone\(\)\)/, "the engine gets the shared set of allowed folders");
  assert.match(read("crates", "path-safety", "src", "folders.rs"), /pub fn validate_user_folder/);
});

// ---------------------------------------------------------------------------------------------
// Opening downloaded files: only the one audited place may start the file manager or open a file

test("only system-integration shows or opens files, never a program, and only by id from the web view", () => {
  // Starting the file manager / "open with the usual application" happens in exactly three audited modules (the
  // third installs a verified update: see the updates test).
  const allowed = ["crates/system-integration/src/files.rs", "crates/system-integration/src/browser_launch.rs", "crates/system-integration/src/update.rs"];
  const bad = [];
  for (const f of shippedRust()) {
    const rel = path.relative(ROOT, f).replaceAll("\\", "/");
    const text = withoutTestModules(fs.readFileSync(f, "utf8"));
    if (/explorer\.exe|"xdg-open"|"\/usr\/bin\/open"|Command::new\("open"\)|ShellExecute|tauri_plugin_opener|opener::/.test(text) && !allowed.includes(rel)) bad.push(rel);
  }
  assert.deepEqual(bad, [], "files are shown / opened through system_integration::files only");
  const files = read("crates", "system-integration", "src", "files.rs");
  // `open` checks for programs, scripts and web pages before anything runs, and only takes absolute paths.
  const open = files.slice(files.indexOf("pub fn open("), files.indexOf("fn explorer("));
  assert.ok(open.indexOf("is_risky_to_open") > 0 && open.indexOf("is_risky_to_open") < open.indexOf("match platform"), "the risk check comes first");
  assert.match(open, /is_absolute/);
  for (const ext of ["exe", "msi", "bat", "cmd", "ps1", "vbs", "js", "lnk", "hta", "scr", "jar", "sh", "app", "desktop", "html", "svg"]) {
    assert.match(files, new RegExp(`"${ext}"`), `${ext} must be in the list of files never opened`);
  }
  // The web view names a download or a history line by id; the path comes from the database.
  const api = read("crates", "download-manager", "src", "api.rs");
  const target = api.slice(api.indexOf("pub enum FileTarget"), api.indexOf("\n}", api.indexOf("pub enum FileTarget")));
  assert.match(target, /Download \{\s*id: i64,?\s*\}/);
  assert.match(target, /History \{\s*id: i64,?\s*\}/);
  assert.match(target, /Media \{\s*id: u64,?\s*\}/, "a video job, by id: its file comes from the job the manager keeps");
  assert.doesNotMatch(target, /String|Path/);
  // ... and the manager checks it lies inside a download folder before showing or opening it.
  const mf = read("crates", "download-manager", "src", "manager", "files.rs");
  assert.match(mf, /self\.inner\.roots\.check\(&path\)/);
  // The desktop app never uses the opener plugin (it would give the web view a way to open anything).
  assert.ok(!lockedCrates().includes("tauri-plugin-opener"));
});

test("proxy and sign-in secrets are sealed, never returned to the web view and never logged", () => {
  const net = read("crates", "download-manager", "src", "manager", "network.rs");
  assert.match(net, /s\.seal\(p\.as_bytes\(\), PASSWORD_CONTEXT\)/, "the proxy password is sealed before it is stored");
  const api = read("crates", "download-manager", "src", "api.rs");
  const dto = api.slice(api.indexOf("pub struct NetworkDto"), api.indexOf("}", api.indexOf("pub struct NetworkDto")));
  assert.doesNotMatch(dto, /password:\s|proxy_password\s*:/, "the network settings sent to the UI carry no password");
  assert.match(dto, /proxy_has_password: bool/);
  // Requests carrying secrets have hand-written Debug that hides them.
  for (const t of ["NewDownload", "SetCredentials", "SetNetwork"]) {
    assert.match(api, new RegExp(`impl std::fmt::Debug for ${t}`), `${t} must not derive Debug`);
  }
});

// ---------------------------------------------------------------------------------------------
// Phase 14: observability stays local and never carries a secret

test("no telemetry, crash-upload or second logging library is linked: the log stays on this computer", () => {
  const banned = [
    "tracing-subscriber", "tracing-appender", "tracing-opentelemetry", "opentelemetry", "opentelemetry_sdk", "opentelemetry-otlp",
    "env_logger", "log4rs", "fern", "simplelog", "flexi_logger", "sentry", "sentry-core", "sentry-tracing", "rollbar", "bugsnag",
    "tauri-plugin-log", "tauri-plugin-aptabase", "posthog-rs", "segment",
  ];
  const present = lockedCrates().filter((c) => banned.includes(c));
  assert.deepEqual(present, [], "every log line must go through crates/diagnostics (redaction), and nothing may be sent anywhere");
});

test("one logger, installed by the desktop app before anything runs, redacts every line", () => {
  // Only the diagnostics crate implements a subscriber or installs one.
  const offenders = [];
  for (const f of shippedRust()) {
    const rel = path.relative(ROOT, f).replaceAll("\\", "/");
    const text = withoutTestModules(fs.readFileSync(f, "utf8"));
    if (/impl\s+(?:tracing::)?Subscriber\s+for|set_global_default/.test(text) && rel !== "crates/diagnostics/src/log.rs") offenders.push(rel);
  }
  assert.deepEqual(offenders, []);
  const log = read("crates", "diagnostics", "src", "log.rs");
  assert.match(log, /self\.0\.redactor\.redact\(&text\)/, "events are redacted before they are kept");
  assert.match(log, /if is_secret_name\(name\)/, "fields named like secrets are masked whatever their value");
  assert.match(log, /fn write_panic/, "crashes are recorded (locally)");
  // The desktop app starts the log first thing in setup, and hands it to the manager.
  const lib = read("apps", "desktop", "src-tauri", "src", "lib.rs");
  const setup = lib.slice(lib.indexOf(".setup(|app|"));
  assert.ok(setup.indexOf("start_log(") > 0 && setup.indexOf("start_log(") < setup.indexOf("DownloadManager::new"), "the log is installed before the manager starts");
  assert.match(setup, /cfg\.log = Some\(log\)/);
  // Reports built for support are redacted once more as a whole.
  const diag = read("crates", "download-manager", "src", "manager", "diag.rs");
  assert.ok((diag.match(/self\.redactor\(\)\.redact\(&r\)/g) ?? []).length >= 2, "both reports pass through the redactor");
});

test("log fields are never named after secrets or carry request headers", () => {
  const bad = [];
  const secretName = /^(authorization|proxy_authorization|cookies?|set_cookie|passwords?|proxy_password|token|access_token|secret|credentials?|headers|request_headers|auth)$/i;
  for (const f of shippedRust()) {
    const text = withoutTestModules(fs.readFileSync(f, "utf8"));
    for (const m of text.matchAll(/tracing::(?:trace|debug|info|warn|error)!\(([\s\S]*?)\);/g)) {
      // `name = value`, `?name`, `%name`
      const names = [...m[1].matchAll(/(?:^|[,(\s])([a-z_][a-z0-9_]*)\s*=(?!=)/gi)].map((x) => x[1]);
      names.push(...[...m[1].matchAll(/[?%]([a-z_][a-z0-9_]*)/gi)].map((x) => x[1]));
      for (const n of names) if (secretName.test(n)) bad.push(`${path.relative(ROOT, f)}: ${n} in ${m[0].slice(0, 90)}`);
    }
  }
  assert.deepEqual(bad, []);
});

test("the Phase 14 suites exist and the diagnostics commands take ids only", () => {
  const t = read("tests", "integration", "tests", "diagnostics.rs");
  assert.match(t, /fn passwords_cookies_tokens_and_proxy_passwords_never_reach_the_log_the_diagnostics_the_reports_or_the_database/);
  assert.ok(exists("tests", "e2e", "desktop-diagnostics.mjs"));
  const lib = read("apps", "desktop", "src-tauri", "src", "lib.rs");
  for (const c of ["get_download_diagnostics, GetDownloadDiagnostics, (id: i64)", "open_log_folder, OpenLogFolder, ()", "get_diagnostics_report, GetDiagnosticsReport, ()"]) {
    assert.ok(lib.includes(c), c);
  }
});

test("Phase 15: the operating system is reached through SystemIntegration only, real only in the app, never blocking a sleep the user asks for", () => {
  // The download manager holds a SystemHandle; it never runs a program itself.
  const bad = [];
  for (const f of shippedRust()) {
    const rel = path.relative(ROOT, f).split(path.sep).join("/");
    if (rel.startsWith("crates/download-manager/") && /SystemPower|SystemShell|std::process::Command::new|tokio::process::Command::new/.test(withoutTestModules(fs.readFileSync(f, "utf8")))) bad.push(rel);
  }
  assert.deepEqual(bad, []);
  const api = read("crates", "download-manager", "src", "api.rs");
  assert.match(api, /system: SystemHandle\(Arc::new\(DryRunSystem::new\(\)\)\)/, "anything but the app gets the dry run by default");
  const lib = read("apps", "desktop", "src-tauri", "src", "lib.rs");
  assert.match(lib, /cfg\.system = download_manager::SystemHandle::native\(\);/);
  assert.ok(lib.includes("command!(set_keep_awake, SetKeepAwake, (enabled: bool)"), "the keep-awake command takes a yes / no only");
  const sys = withoutTestModules(read("crates", "system-integration", "src", "system.rs"));
  assert.ok(!/Command::new\("(sh|bash|cmd|cmd\.exe|powershell|powershell\.exe)"\)/.test(sys), "no shell");
  assert.match(sys, /"--what=idle"/, "Linux: only the automatic sleep after inactivity is held off");
  assert.match(sys, /"-i"\.into\(\), "-w"\.into\(\), pid\.to_string\(\)/, "macOS: idle sleep only, and it ends with the app");
  assert.match(sys, /"on run argv"/, "macOS notification text travels as argv, never inside the script");
  assert.match(sys, /"--"\.into\(\), n\.title\.clone\(\), n\.body\.clone\(\)/, "Linux notification text comes after --");
  assert.match(sys, /xml_escape\(&n\.title\)/, "Windows toast text is escaped");
});

test("Phases 16-18: tray, start at login and notifications stay per user, opt-in where it matters, and never widen what the UI can do", () => {
  const lib = read("apps", "desktop", "src-tauri", "src", "lib.rs");
  const desk = withoutTestModules(read("apps", "desktop", "src-tauri", "src", "desktop.rs"));
  const mgr = withoutTestModules(read("crates", "download-manager", "src", "manager", "desktop.rs"));
  const api = read("crates", "download-manager", "src", "api.rs");
  const sys = withoutTestModules(read("crates", "system-integration", "src", "system.rs"));
  // Debug builds (and every test that runs them) never touch the machine's login items.
  assert.match(lib, /cfg\.app_launcher = if cfg!\(debug_assertions\) \{ None \} else \{ desktop::launcher\(\) \};/);
  // The settings command takes yes / no answers and a language code, nothing else (no path, no program).
  assert.match(api, /#\[serde\(rename_all = "camelCase", deny_unknown_fields\)\]\s*pub struct SetDesktopPrefs \{\s*pub close_to_tray: Option<bool>,\s*pub start_at_login: Option<bool>,\s*pub notify_finished: Option<bool>,[\s\S]{0,80}pub language: Option<String>,\s*\}/);
  assert.match(mgr, /if !valid_language\(lang\)/, "the language is checked before it is stored");
  assert.match(mgr, /if self\.flag\(START_AT_LOGIN_KEY, false\) && !system\.starts_at_login\(exe\)/, "start at login is only kept up to date once the user turned it on (off by default)");
  // Start at login runs only the app itself, hidden in the tray; macOS never restarts it after the user quits.
  assert.match(sys, /format!\("\\"\{\}\\" \{HIDDEN_ARG\}", exe\.display\(\)\)/, "Windows Run value: the quoted app and --hidden");
  assert.match(sys, /<key>KeepAlive<\/key><false\/>/);
  assert.match(sys, /fn check_login_exe/, "the program path is checked (absolute, no quotes or new lines)");
  assert.match(sys, /pub fn body_text\(body: &str\)/, "Linux notification bodies are escaped (servers read markup)");
  // The tray library is looked for before the tray code (which stops the program when it is missing) runs.
  assert.ok(desk.indexOf("if !tray_possible()") < desk.indexOf("TrayIconBuilder::with_id"), "tray checked before it is made");
  // A desktop without a tray never hides the window: closing then quits.
  assert.match(desk, /!prefs\.close_to_tray \|\| !prefs\.tray_available/);
  // Only Rust shows the window (so a run started hidden at login stays in the tray): the UI cannot show or focus it.
  const caps = read("apps", "desktop", "src-tauri", "capabilities", "main-window.json");
  assert.ok(!caps.includes("core:window:allow-show") && !caps.includes("core:window:allow-set-focus"));
  // The show signal of a second launch carries nothing and is the user's only.
  assert.match(desk, /from_mode\(0o600\)/);
  // macOS: hardened runtime, no entitlement that weakens it.
  const ent = read("apps", "desktop", "src-tauri", "packaging", "macos", "Entitlements.plist");
  assert.match(ent, /<dict\/>/, "no entitlements at all");
  const mac = JSON.parse(read("apps", "desktop", "src-tauri", "tauri.release.macos.conf.json"));
  assert.equal(mac.bundle.macOS.hardenedRuntime, true);
  // Signing secrets reach only the steps that import them; the build job never prints them.
  const build = read(".github", "workflows", "build.yml");
  assert.match(build, /if: runner\.os == 'Windows' && env\.WINDOWS_CERT_PFX_BASE64 != ''/);
  assert.match(build, /if: runner\.os == 'macOS' && env\.APPLE_CERTIFICATE != ''/);
  assert.ok(!/echo "\$(WINDOWS_CERT|APPLE_CERTIFICATE|APPLE_PASSWORD)/.test(build.replace(/echo "\$APPLE_CERTIFICATE" \| base64 --decode/, "")), "no secret is echoed to the log");
});

test("Smart Download Queue: main window only, ids and plain rules only, nothing is sent to find out the connection", () => {
  const api = read("crates", "download-manager", "src", "api.rs");
  const rules = read("crates", "scheduler", "src", "queue_rules.rs");
  const queues = withoutTestModules(read("crates", "download-manager", "src", "manager", "queues.rs"));
  const net = withoutTestModules(read("crates", "system-integration", "src", "network.rs"));
  const authz = read("apps", "desktop", "src-tauri", "src", "authz.rs");
  // The five commands are the main window's only (the detail and settings windows cannot reach them).
  for (const c of ["get_queues", "save_queue", "delete_queue", "queue_action", "place_download"]) {
    assert.match(read("apps", "desktop", "src-tauri", "build.rs"), new RegExp(`"${c}"`));
    assert.match(read("apps", "desktop", "src-tauri", "capabilities", "main-window.json"), new RegExp(`"allow-${c.replaceAll("_", "-")}"`));
    for (const other of ["download-window.json", "settings-window.json"]) {
      assert.ok(!read("apps", "desktop", "src-tauri", "capabilities", other).includes(`allow-${c.replaceAll("_", "-")}`), `${c} not in ${other}`);
    }
  }
  assert.match(authz, /Command::PlaceDownload,/);
  // Requests are read strictly: no path, no program, no command; only ids, names, times and limits.
  for (const name of ["SaveQueue", "PlaceDownload"]) {
    assert.match(api, new RegExp(`#\\[serde\\(rename_all = "camelCase", deny_unknown_fields\\)\\]\\s*pub struct ${name} \\{`));
  }
  assert.match(api, /#\[serde\(tag = "kind", rename_all = "camelCase", deny_unknown_fields\)\]\s*pub enum QueueAction/);
  assert.match(rules, /#\[serde\(rename_all = "camelCase", deny_unknown_fields, default\)\]\s*pub struct QueueRules/);
  assert.match(queues, /req\.rules\.validate\(\)/, "rules are validated before they are saved");
  assert.match(queues, /MAX_QUEUES: usize = 16/);
  assert.match(queues, /MAX_NAME: usize = 40/);
  // Something unknown never holds a download back; the queues never touch the power action.
  assert.match(rules, /if let Some\(net\) = inp\.net/);
  assert.ok(!/power/i.test(queues.replace(/power action/gi, "")), "the queues do not reach the power action");
  // Finding out the connection reads the routing table and the adapter only: no request is made.
  assert.ok(!/reqwest|TcpStream|UdpSocket|http_client|\.send\(/.test(net), "no socket and no request to find out the connection");
});

test("Download Intelligence: main window only, a link only, read-only, one quick check, no secret in the result", () => {
  const rules = withoutTestModules(read("crates", "download-manager", "src", "intelligence.rs"));
  const glue = withoutTestModules(read("crates", "download-manager", "src", "manager", "intelligence.rs"));
  const pending = read("crates", "download-manager", "src", "manager", "pending.rs");
  // The Add dialog's check is the main window's only.
  assert.match(read("apps", "desktop", "src-tauri", "build.rs"), /"analyze_link"/);
  assert.match(read("apps", "desktop", "src-tauri", "capabilities", "main-window.json"), /"allow-analyze-link"/);
  for (const other of ["download-window.json", "settings-window.json"]) {
    assert.ok(!read("apps", "desktop", "src-tauri", "capabilities", other).includes("allow-analyze-link"), `analyze_link not in ${other}`);
  }
  assert.match(read("apps", "desktop", "src-tauri", "src", "authz.rs"), /Command::AnalyzeLink,/);
  // The request is a link, read strictly.
  assert.match(rules, /#\[serde\(rename_all = "camelCase", deny_unknown_fields\)\]\s*pub struct AnalyzeLink \{\s*pub url: Option<String>,\s*\}/);
  // The rules do no I/O at all; the glue only reads: no file is created, written, moved or deleted.
  assert.ok(!/std::fs|tokio::fs|reqwest|\.send\(|\.await|async fn/.test(rules), "the rules are pure");
  assert.ok(!/OpenOptions|File::create|fs::write|remove_file|remove_dir|rename\(|create_dir|create_preallocated|set_setting|\.save\(/.test(glue), "the analysis changes nothing");
  // One bounded quick check (HEAD + one byte), never the file itself.
  assert.match(glue, /tokio::time::timeout\(ANALYSIS_TIMEOUT, self\.inner\.downloader\.probe_quick\(/);
  assert.ok(!/get_with|get_patient|get_range|\.download\(/.test(glue), "nothing is downloaded to analyse it");
  // No secret reaches the result or a log: failures become a status or a code, the shown link hides signatures.
  assert.match(glue, /ProbeFailure::Other\(crate::explain::error_code\(&e\.to_string\(\)\)\)/);
  assert.ok(!/tracing::/.test(glue) && !/tracing::/.test(rules), "the analysis logs nothing");
  assert.match(rules, /pub final_host: Option<String>/, "only the host the link led to, not the address");
  assert.ok(!/final_url: /.test(rules.slice(rules.indexOf("pub struct AnalysisDto"), rules.indexOf("pub enum ProbeFailure"))), "no full final address in the result");
  assert.match(pending, /url: crate::intelligence::display_url\(&url\)/);
  // Answering stops the analysis.
  assert.match(pending, /task\.abort\(\); \/\/ answered/);
});

test("the publisher's checksum: main window only, https next to the file, no cookie or sign-in, bounded, never called safe", () => {
  const lookup = withoutTestModules(read("crates", "download-manager", "src", "manager", "publisher.rs"));
  const lists = withoutTestModules(read("crates", "downloader-core", "src", "integrity.rs"));
  const app = read("apps", "desktop", "src-tauri", "src", "lib.rs");
  // The switch is the main window's only.
  for (const c of ["get_publisher_check", "set_publisher_check"]) {
    assert.match(read("apps", "desktop", "src-tauri", "build.rs"), new RegExp(`"${c}"`));
    assert.match(read("apps", "desktop", "src-tauri", "capabilities", "main-window.json"), new RegExp(`"allow-${c.replaceAll("_", "-")}"`));
    for (const other of ["download-window.json", "settings-window.json"]) {
      assert.ok(!read("apps", "desktop", "src-tauri", "capabilities", other).includes(c.replaceAll("_", "-")), `${c} not in ${other}`);
    }
  }
  // https only; plain http only through the debug-only test switch (a release never reads it).
  assert.match(lookup, /url\.scheme\(\) == "https" \|\| over_http && url\.scheme\(\) == "http"/);
  assert.match(read("crates", "download-manager", "src", "api.rs"), /publisher_over_http: false,/);
  assert.match(app, /if dev_only\("VELOX_PUBLISHER_HTTP"\)\.is_some\(\) \{\s*cfg\.publisher_over_http = true;/);
  assert.equal((app.match(/publisher_over_http/g) ?? []).length, 1, "set in one place only");
  // The lists: the protected client, a plain GET (no cookie, sign-in, referrer or header), the same host, never the
  // link's query, bounded in size and time.
  assert.match(lookup, /self\.media_client\(\)\.get\(url\)/);
  const code = lookup.split("\n").filter((l) => !l.trim().startsWith("//")).join("\n");
  assert.ok(!/get_with|RequestExtras|cookie|authorization|referer|header\(/i.test(code.replace(/headers\(\)\.get\("content-type"\)/, "")), "nothing private goes with a list");
  assert.match(lookup, /folder\.set_query\(None\);/);
  assert.match(lookup, /resp\.url\(\)\.host_str\(\) != url\.host_str\(\)/);
  assert.match(lookup, /const MAX_LIST: usize = 1024 \* 1024;/);
  assert.match(lookup, /tokio::time::timeout\(LOOKUP_BUDGET, self\.ask_lists\(url\)\)/);
  assert.match(lookup, /tokio::time::timeout\(LIST_TIMEOUT, read\)/);
  assert.match(lists, /take\(MAX_LIST_LINES\)/);
  // Nothing is written but the setting; the list's address is never logged or kept.
  assert.ok(!/OpenOptions|File::create|fs::write|remove_file|rename\(|create_dir/.test(lookup), "the lookup writes no file");
  assert.ok(!/tracing::[a-z]+!\([^;]*url/.test(lookup), "no address in the log");
  // Never "safe": the interface says what it proves.
  const en = JSON.parse(read("apps", "desktop", "src", "locales", "en.json"));
  const texts = Object.entries(en).filter(([k]) => k.startsWith("verify.")).map(([, v]) => v);
  assert.ok(texts.length > 10 && texts.every((t) => !/\bsafe\b/i.test(t)), "never called safe");
  assert.match(en["verify.setLimit"], /not that the site can be trusted/);
});

test("Download Doctor: diagnoses only, never retries or writes by itself, no error text or secret kept", () => {
  const rules = withoutTestModules(read("crates", "download-manager", "src", "doctor.rs"));
  const glue = withoutTestModules(read("crates", "download-manager", "src", "manager", "doctor.rs"));
  // The rules are pure.
  assert.ok(!/std::fs|tokio::|reqwest|\.send\(|\.await|async fn/.test(rules), "the rules do no I/O");
  // The glue only reads: no retry, start, write or request of its own (the engine's retry loop stays the only one).
  assert.ok(!/retry_download|start_download|with_retry|probe|get_with|\.save\(|patch_saved|set_setting|remove_file|tokio::fs/.test(glue), "the Doctor changes nothing");
  // Transport errors are read from their causes, never from the top message (it names the link); nothing is logged.
  assert.match(glue, /let mut src = t\.source\(\);/);
  assert.ok(!/tracing::/.test(glue), "the Doctor logs nothing");
  // A diagnosis holds codes, classes, counts and byte numbers only: no free text that could carry a link or a token.
  const dto = rules.slice(rules.indexOf("pub struct Diagnosis"), rules.indexOf("pub struct Evidence"));
  assert.ok(!/: String,/.test(dto.replace("pub code: String,", "")), "no free text in a diagnosis");
  // Certificate failures are not retried (they do not fix themselves).
  assert.match(read("crates", "http-client", "src", "lib.rs"), /HttpError::Transport\(e\) => !\(e\.is_builder\(\) \|\| e\.is_redirect\(\) \|\| tls_will_fail_again\(e\)\)/);
});

test("video tools are fetched only from fixed https sources, checked against the published checksum before use, one fixed entry out of an archive", () => {
  const tools = withoutTestModules(read("crates", "download-manager", "src", "manager", "tools.rs"));
  // The only sources are the official ones, compiled in (the UI can never name one).
  const official = tools.slice(tools.indexOf("pub fn official()"), tools.indexOf("pub fn official()") + 700);
  for (const host of official.match(/https?:\/\/[^"]+/g) ?? []) {
    assert.ok(/^https:\/\/(github\.com\/yt-dlp\/|dl\.deno\.land|www\.gyan\.dev\/ffmpeg\/|ffmpeg\.martin-riedl\.de\/)/.test(host), `unexpected tool source ${host}`);
  }
  assert.match(tools, /fn secure\(u: url::Url\)/, "plain http only for local test servers");
  assert.match(tools, /if got != expected \{\s*return Err\("the file does not match its published checksum"/, "the hash is compared before anything is unpacked or put in place");
  assert.ok(tools.indexOf("if got != expected") < tools.indexOf("extract_entry(&download2"), "checksum first, then unpack");
  assert.match(tools, /name == wanted_lower \|\| name\.ends_with/, "an archive entry is found by a fixed name; its path is never used");
  assert.match(tools, /\[\(method, csize, size, crc, local\)\] = found\.as_slice\(\)/, "exactly one matching entry");
  assert.match(tools, /crc32\.sum\(\) != \*crc/, "the entry's CRC is checked");
  assert.ok(!/Command::new/.test(tools), "the installer starts nothing");
  // No UI command can choose a source or a path: two commands take nothing, the answer to an update a yes or a no.
  const lib = read("apps", "desktop", "src-tauri", "src", "lib.rs");
  assert.match(lib, /command!\(fetch_video_tools, FetchVideoTools, \(\) -> VideoToolsDto/);
  assert.match(lib, /command!\(answer_video_tools_update, AnswerVideoToolsUpdate, \(accept: bool\) -> VideoToolsDto/);
});

test("the installer's copies of the video tools: fixed names, checked before use, the list pinned in the program", () => {
  const tools = withoutTestModules(read("crates", "download-manager", "src", "manager", "tools.rs"));
  // The compressed file is opened by a fixed name: nothing in the list names a file.
  assert.match(tools, /let packed = dir\.join\(format!\("\{\}\.xz", tool\.file_name\(\)\)\);/);
  // The list is for this computer, and its SHA-256 is compiled into release builds.
  assert.match(tools, /manifest\.os != std::env::consts::OS \|\| manifest\.arch != std::env::consts::ARCH/);
  assert.match(tools, /option_env!\("VELOX_VIDEO_TOOLS_SHA256"\)/);
  assert.match(tools, /if !to_hex\(&Sha256::digest\(&text\)\)\.eq_ignore_ascii_case\(pinned\)/);
  // Unpacked to a temporary name; size and SHA-256 checked; only then made runnable and put in place.
  assert.match(tools, /if written != size \|\| hash != want \{\s*return Err\(bad\("the unpacked program is damaged"\)\);/);
  assert.match(tools, /unpack_xz\(&packed, &fresh, entry\.size, &entry\.sha256, got\)\s*\.and_then\(\|\(\)\| make_runnable\(&fresh\)\)\s*\.and_then\(\|\(\)\| std::fs::rename\(&fresh, &target\)\)/);
  assert.match(tools, /XzReader::new_mem_limit\(input, false, UNPACK_MEMORY_KB\)/, "the decoder's memory is bounded");
  // The decoder is safe Rust only, and the shipped program carries no encoder.
  const cargo = read("crates", "download-manager", "Cargo.toml");
  const dep = cargo.slice(0, cargo.indexOf("[dev-dependencies]")).match(/^lzma-rust2 = .*$/m)?.[0] ?? "";
  assert.match(dep, /default-features = false, features = \["std", "xz"\]/, dep);
  // The release build pins the list it made.
  const build = read("scripts", "build-release.sh");
  assert.match(build, /export VELOX_VIDEO_TOOLS_SHA256/);
  assert.ok(build.indexOf("export VELOX_VIDEO_TOOLS_SHA256") < build.indexOf("tauri -- build"), "pinned before the app is compiled");
});

test("video tool updates are offered, never downloaded or installed by themselves", () => {
  const tools = withoutTestModules(read("crates", "download-manager", "src", "manager", "tools.rs"));
  const updates = withoutTestModules(read("crates", "download-manager", "src", "manager", "tool_updates.rs"));
  const media = withoutTestModules(read("crates", "download-manager", "src", "manager", "media.rs"));
  // Nothing replaces an installed tool from the internet on its own: no refresh at start-up or from a video page.
  const prefetch = tools.slice(tools.indexOf("pub fn prefetch_video_tools"), tools.indexOf("/// Fetch every missing tool now"));
  assert.ok(!/, true\)/.test(prefetch), "start-up only fills in what is missing");
  assert.ok(!/ensure_tool\(Tool::YtDlp, true\)|fetch_in_background\(Tool::YtDlp, true\)/.test(media), "a video page only asks for a check");
  // An update is installed only from the user's answer.
  assert.equal((updates.match(/self\.install\(/g) ?? []).length, 1);
  assert.match(updates, /async fn apply_offer\(&self, offer: Offer\)[\s\S]*let result = self\.install\(tool\)\.await;/);
  assert.match(updates, /if accept \{\s*for offer in offers \{/);
  assert.equal((updates.match(/\.apply_offer\(/g) ?? []).length, 1, "only from an accepted answer");
  // The checks read the same fixed sources, over https (join -> secure), and send nothing about the user.
  assert.ok(!/https?:\/\//.test(updates), "no address of its own");
  assert.ok(!/Command::new/.test(updates));
});

test("only ffmpeg and yt-dlp are ever started by the media engine, both found by absolute path", () => {
  const bad = [];
  for (const f of shippedRust()) {
    const rel = path.relative(ROOT, f).split(path.sep).join("/");
    const text = withoutTestModules(fs.readFileSync(f, "utf8"));
    if (/tokio::process::Command::new|std::process::Command::new/.test(text) && rel.startsWith("crates/media-engine/") && !["crates/media-engine/src/ffmpeg.rs", "crates/media-engine/src/site.rs"].includes(rel)) bad.push(rel);
  }
  assert.deepEqual(bad, []);
  const ff = read("crates", "media-engine", "src", "ffmpeg.rs");
  assert.match(ff, /filter\(\|p\| p\.is_absolute\(\)\)/, "PATH entries that are not absolute (the current folder) are skipped");
  const site = read("crates", "media-engine", "src", "site.rs");
  assert.match(site, /crate::ffmpeg::find_program\(/, "yt-dlp is found the same way as ffmpeg");
});

test("yt-dlp: no configuration files, no --exec, never cookies, the link last after --, DRM refused", () => {
  const site = withoutTestModules(read("crates", "media-engine", "src", "site.rs"));
  const base = site.slice(site.indexOf("fn base_args"), site.indexOf("fn push_url"));
  for (const opt of ["--ignore-config", "--no-exec", "--no-playlist", "--no-batch-file"]) assert.ok(base.includes(`"${opt}"`), opt);
  assert.match(site.slice(site.indexOf("fn push_url")), /a\.push\(OsString::from\("--"\)\);\s*a\.push\(OsString::from\(url\.as_str\(\)\)\);/);
  assert.ok(!/"--cookies|"--exec"|"--load-info-json"|cookies-from-browser/.test(site), "no cookie, exec or info-json option is ever built");
  assert.match(site, /fn protected_service/);
  const media = read("crates", "download-manager", "src", "manager", "media.rs");
  assert.ok((media.match(/drm_refusal\(&url\)\?/g) ?? []).length >= 2, "both looking and downloading refuse DRM services first");
});

test("live preview: main window only, a download id only, media types only, nosniff", () => {
  const srv = withoutTestModules(read("apps", "desktop", "src-tauri", "src", "preview_server.rs"));
  assert.match(srv, /ctx\.webview_label\(\) != WINDOW/);
  assert.match(srv, /const WINDOW: &str = "main";/);
  assert.match(srv, /target_from_path\(request\.uri\(\)\.path\(\)\)/, "the path is only ever read as an id");
  const tf = srv.slice(srv.indexOf("pub fn target_from_path"), srv.indexOf("pub fn handle"));
  assert.ok(tf.includes("id_from_path") && !/PathBuf|Path::new|join\(/.test(tf), "a download id (/12) or a video job id (/m3), digits only");
  assert.match(srv, /"X-Content-Type-Options", "nosniff"/);
  assert.match(srv, /CONTENT_SECURITY_POLICY, "default-src 'none'"/);
  const pv = withoutTestModules(read("crates", "download-manager", "src", "manager", "preview.rs"));
  assert.match(pv, /self\.inner\.roots\.check\(&path\)/, "only files inside a download folder");
  const mp = pv.slice(pv.indexOf("pub async fn read_media_preview"));
  assert.ok(mp.includes("self.inner.roots.check(&path)") && mp.includes("media_type("), "video jobs too: inside a download folder, audio / video only");
  assert.match(pv, /pub fn media_type/);
  assert.ok(!/text\/html|application\/|image\/svg/.test(pv.slice(pv.indexOf("pub fn media_type"), pv.indexOf("pub fn available_runs"))), "only audio / video types");
});

test("smart throttle: process and executable names only (paths only when listed), nothing stored or sent, full speed soon after any failure", () => {
  const cargo = read("apps", "desktop", "src-tauri", "Cargo.toml");
  assert.match(cargo, /sysinfo = \{ version = "[^"]+", default-features = false, features = \["system"\] \}/);
  const st = withoutTestModules(read("apps", "desktop", "src-tauri", "src", "smart_throttle.rs"));
  assert.match(st, /ProcessRefreshKind::nothing\(\)\.with_exe\(UpdateKind::OnlyIfNotSet\)/, "no CPU, memory, command line or environment of other programs; the executable read once");
  assert.ok(!/tracing::|println!|eprintln!|cmd\(\)|environ|\.cwd\(\)|\.root\(\)|\.user_id\(\)/.test(st), "process details are never logged or read beyond names");
  assert.match(st, /p\.exe\(\)\.and_then\(\|e\| e\.file_name\(\)\)/, "the executable's file name");
  assert.match(st, /let path = p\.exe\(\)\.filter\(\|_\| paths\)/, "full paths only when the user listed a path");
  assert.match(st, /if !matches!\(tokio::time::timeout\(SCAN_LIMIT, scan\)\.await, Ok\(Ok\(\(\)\)\)\) \{[\s\S]{0,120}manager\.smart_throttle_observe\(None\);/, "a failed, slow or panicking scan means full speed");
  const core = read("crates", "download-manager", "src", "manager", "smart_throttle.rs");
  assert.match(core, /enabled: get\(ENABLED_KEY\)\.as_deref\(\) == Some\("true"\)/, "off unless the user turned it on");
  // Gaming policy: a failed scan keeps it for a bounded time only; the queue hold lives in memory (a restart never
  // leaves downloads held); the low limit can never be zero (zero would mean "no limit").
  assert.match(core, /pub const SCAN_FAIL_GRACE: Duration = Duration::from_secs\(\d{1,2}\);/, "a failed scan lifts the policy within seconds");
  assert.match(core, /if now\.duration_since\(since\) >= SCAN_FAIL_GRACE \{\s*self\.active = None;/);
  assert.match(core, /req\.game_limit_bps \{\s*if !\(MIN_FLOOR_BPS\.\.=MAX_FLOOR_BPS\)\.contains\(&m\)/, "the gaming limit is bounded");
  const qu = read("crates", "download-manager", "src", "manager", "queue.rs");
  assert.ok(!/GAME_HOLD_KEY|"queue\.game/.test(qu), "the game hold is never saved");
  assert.match(qu, /!self\.manual_paused && !self\.game_hold && self\.schedule\.is_open\(now\)/);
  // Automatic mode: of the keyboard and mouse only the idle time is known; traffic is a total, never per program
  // or per site; no counters means full speed; downloads never go below the reserved minimum.
  const sys = withoutTestModules(read("crates", "system-integration", "src", "system.rs"));
  assert.match(sys, /fn user_idle\(&self\) -> Option<std::time::Duration>;/);
  assert.ok(!/SetWindowsHookEx|GetAsyncKeyState|CGEventTapCreate|XRecord|\/dev\/input/.test(sys), "no input is ever read, only the idle time");
  assert.match(core, /let Some\(raw\) = sig\.other_bps else \{[\s\S]{0,160}return \(None, Reason::Unknown\);/, "no counters: full speed");
  assert.match(core, /\(known \* keep\)\.max\(floor\)/, "the automatic cap respects the reserved minimum");
  // The other traffic is measured from totals only (the computer's received bytes, VeloxFetch's own), never per
  // program or per connection.
  const io = withoutTestModules(read("crates", "download-manager", "src", "manager", "sysio.rs"));
  assert.match(io, /pub\(super\) fn net_received_total\(&self\) -> Option<u64>/);
  assert.ok(!/processes|ProcessRefreshKind|sysinfo::.*Sockets|connections\(/.test(io), "no per-program or per-connection traffic");
  // Linux: the bytes waiting in VeloxFetch's *own* receive buffers (Smart throttle's measurement). Only this process's
  // sockets are summed (matched by their inodes from /proc/self/fd); other entries of the table are skipped, never kept.
  assert.match(io, /read_dir\("\/proc\/self\/fd"\)/);
  assert.match(io, /if !inodes\.contains\(&inode\) \{\s*return None;/);
  assert.ok(!/\/proc\/\[0-9\]|\/proc\/\{|rem_address\s*=|local_address\s*=/.test(io), "no other process, no addresses kept");
  assert.match(core, /MIN_FLOOR_BPS: u64 = 32 \* 1024;/, "the reserved minimum can never be zero: downloads never stop");
});

test("linux packages: media plugins never block the install, the AppImage never carries hardware plugins", { skip: process.platform === "win32" && "needs bash" }, async () => {
  // .deb / .rpm: a media package the system cannot install (mixed or pinned repositories) must not stop the app
  // from installing; the preview then says "can't play" for that format.
  const linux = JSON.parse(read("apps", "desktop", "src-tauri", "tauri.release.linux.conf.json")).bundle.linux;
  for (const kind of ["deb", "rpm"]) {
    const hard = (linux[kind].depends ?? []).filter((d) => /gst|libav|ffmpeg/i.test(d));
    assert.deepEqual(hard, [], `${kind}: media packages are recommended, never required`);
    assert.ok(linux[kind].recommends.some((d) => /libav/.test(d)), `${kind}: the codecs are still recommended`);
  }
  // AppImage: only the allow-listed software plugins are handed to the bundler.
  const build = read("scripts", "build-release.sh");
  assert.match(build, /\. scripts\/appimage-media\.sh/);
  assert.match(build, /stage_appimage_gst_plugins "\$\(gst_plugins_dir\)"/);
  assert.match(build, /export GSTREAMER_PLUGINS_DIR="\$PWD\/\$gst_staged"/);
  assert.doesNotMatch(build, /GSTREAMER_PLUGINS_DIR="\$\(gst_plugins_dir\)"/, "never the whole system folder");

  const { execFileSync } = await import("node:child_process");
  const os = await import("node:os");
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "velox-gst-"));
  try {
    const src = path.join(tmp, "system");
    fs.mkdirSync(src);
    const hardware = ["va", "vaapi", "nvcodec", "qsv", "msdk", "v4l2codecs", "video4linux2", "vulkan", "kms", "pipewire", "soup", "onnx"];
    const software = ["coreelements", "typefindfunctions", "playback", "app", "audioconvert", "isomp4", "libav", "matroska", "vpx"];
    for (const n of [...hardware, ...software, "x264", "festival"]) fs.writeFileSync(path.join(src, `libgst${n}.so`), n);
    const stage = (from, to) =>
      execFileSync("bash", ["-c", '. "$1"; stage_appimage_gst_plugins "$2" "$3"', "-", path.join(ROOT, "scripts", "appimage-media.sh"), from, to], { encoding: "utf8" }).trim();
    const dst = path.join(tmp, "staged");
    assert.equal(stage(src, dst), "", "every required plugin found");
    assert.deepEqual(fs.readdirSync(dst).sort(), software.map((n) => `libgst${n}.so`).sort(), "software playback only");
    // Even a list edited by mistake never lets a hardware plugin through.
    const list = execFileSync("bash", ["-c", '. "$1"; for n in va nvcodec qsv vulkan kms v4l2codecs pipewire; do [[ "$n" =~ $APPIMAGE_GST_DENY_RE ]] || echo "$n"; done', "-", path.join(ROOT, "scripts", "appimage-media.sh")], { encoding: "utf8" });
    assert.equal(list.trim(), "", "the deny rule covers the hardware plugins");
    fs.rmSync(path.join(src, "libgstlibav.so"));
    assert.equal(stage(src, dst), "libav", "a missing required plugin is reported (the AppImage is then built without the bundle)");
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test("one version everywhere (the updater compares it; the extension reports it)", () => {
  const v = read("Cargo.toml").match(/^version = "([^"]+)"/m)[1];
  assert.match(v, /^\d+\.\d+\.\d+$/, "x.y.z only: the updater refuses anything else");
  const json = (...p) => JSON.parse(read(...p));
  for (const [name, got] of [
    ["tauri.conf.json", json("apps", "desktop", "src-tauri", "tauri.conf.json").version],
    ["apps/desktop/package.json", json("apps", "desktop", "package.json").version],
    ["apps/desktop/package-lock.json", json("apps", "desktop", "package-lock.json").version],
    ["apps/browser-extension/package.json", json("apps", "browser-extension", "package.json").version],
    ["manifest.chrome.json", json("apps", "browser-extension", "src", "manifest.chrome.json").version],
    ["manifest.firefox.json", json("apps", "browser-extension", "src", "manifest.firefox.json").version],
  ]) {
    assert.equal(got, v, `${name} says ${got}, Cargo.toml says ${v}`);
  }
});

test("Game mode: main window only, bounded, strict requests, nothing new looked at or sent", () => {
  // (This file has a test-only helper near the top: cut at its test module instead.)
  const smartAll = read("crates", "download-manager", "src", "manager", "smart_throttle.rs");
  const smart = smartAll.slice(0, smartAll.indexOf("#[cfg(test)]\nmod tests"));
  const api = read("crates", "download-manager", "src", "api.rs");
  const desktop = withoutTestModules(read("apps", "desktop", "src-tauri", "src", "desktop.rs"));
  // The switch is the main window's only (the tray item runs in the app itself, not through a command).
  assert.match(read("apps", "desktop", "src-tauri", "build.rs"), /"set_game_mode"/);
  assert.match(read("apps", "desktop", "src-tauri", "capabilities", "main-window.json"), /"allow-set-game-mode"/);
  for (const other of ["download-window.json", "settings-window.json"]) {
    assert.ok(!read("apps", "desktop", "src-tauri", "capabilities", other).includes("game-mode"), `set_game_mode not in ${other}`);
  }
  // A strict request: unknown fields refused, at most a day, never longer than a day from now.
  const req = api.slice(api.indexOf("pub struct SetGameMode") - 120, api.indexOf("pub struct SetGameMode"));
  assert.match(req, /deny_unknown_fields/);
  assert.match(smart, /pub const MAX_GAME_MODE_MINUTES: u32 = 24 \* 60;/);
  assert.match(smart, /\.min\(now \+ i64::from\(MAX_GAME_MODE_MINUTES\) \* 60_000\)/);
  // A saved end that cannot be read is off (never "on for ever" by accident); one that has passed is forgotten.
  assert.match(smart, /ms if ms > 0 => Some\(GameUntil::At\(ms\)\),\s*_ => None,/);
  assert.match(smart, /\.filter\(\|g\| !g\.ended\(now_ms\(\)\)\)/);
  assert.match(smart, /delete_setting\(GAME_MODE_UNTIL_KEY\)/);
  // Nothing new is looked at: Game mode does not scan programs or send anything; the end notification is local.
  const game = smart.slice(smart.indexOf("pub async fn set_game_mode"), smart.indexOf("pub(super) fn resume_game_mode"));
  assert.ok(!/http|client|send|running_programs|sysinfo/i.test(game.replace(/\/\/.*$/gm, "")), "Game mode only changes the cap and the queue");
  assert.match(desktop, /notify\(manager, &text\(&prefs\.language, "gm\.endedTitle"\), &text\(&prefs\.language, "gm\.endedBody"\)\)/);
});

test("the installers point the browsers at the store extension the app trusts, at the documented places only, and remove it", () => {
  const id = /CHROME_STORE_ID: Option<&str> = Some\("([a-p]{32})"\)/.exec(read("crates/browser-bridge/src/generated/identity.rs"))?.[1];
  assert.ok(id, "CHROME_STORE_ID is set");
  const store = read("crates/system-integration/src/store_extension.rs");
  const folders = [.../LINUX_SYSTEM_FOLDERS: \[&str; 3\] =\s*\[([^\]]+)\]/.exec(store)[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]);
  const keys = [.../WINDOWS_MACHINE_KEYS: \[&str; 3\] =\s*\[([^\]]+)\]/.exec(store)[1].matchAll(/r"([^"]+)"/g)].map((m) => m[1]);
  assert.equal(folders.length, 3);
  assert.equal(keys.length, 3);
  const update = "https://clients2.google.com/service/update2/crx";

  // Linux packages: exactly the shared folders, the id the app trusts, only a pointer to the store.
  const postinst = read("apps/desktop/src-tauri/packaging/postinst.sh");
  const postrm = read("apps/desktop/src-tauri/packaging/postrm.sh");
  for (const s of [postinst, postrm]) {
    assert.match(s, new RegExp(`^VELOX_EXTENSION_ID=${id}$`, "m"));
    assert.match(s, new RegExp(`for dir in ${folders.join(" ")}; do`));
  }
  assert.ok(postinst.includes(`{"external_update_url":"${update}"}`));
  assert.ok(!/external_crx|--load-extension|ExtensionInstallForcelist|policies/.test(postinst), "never a local package or a policy");
  assert.match(postrm, /remove\|purge\|0\)[\s\S]*rm -f "\$dir\/\$VELOX_EXTENSION_ID\.json"/, "removed with the program, not on upgrade");

  // Windows setup: the same keys under HKLM in the browsers' (32-bit) view, removed on uninstall but not on update.
  const nsh = read("apps/desktop/src-tauri/installer-hooks.nsh").replace(/\r/g, "");
  assert.ok(nsh.includes(`!define VELOX_EXTENSION_ID "${id}"`));
  assert.ok(nsh.includes(`!define VELOX_STORE_UPDATE_URL "${update}"`));
  const install = /!macro NSIS_HOOK_POSTINSTALL([\s\S]*?)!macroend/.exec(nsh)[1];
  const uninstall = /!macro NSIS_HOOK_POSTUNINSTALL([\s\S]*?)!macroend/.exec(nsh)[1];
  for (const k of keys) {
    assert.ok(install.includes(`WriteRegStr HKLM "${k}\\\${VELOX_EXTENSION_ID}" "update_url" "\${VELOX_STORE_UPDATE_URL}"`), k);
    assert.ok(uninstall.includes(`DeleteRegKey HKLM "${k}\\\${VELOX_EXTENSION_ID}"`), k);
  }
  assert.match(install, /SetRegView 32[\s\S]*Extensions[\s\S]*VELOX_REG_VIEW_BACK/);
  assert.match(uninstall, /\$\{If\} \$UpdateMode <> 1[\s\S]*SetRegView 32[\s\S]*DeleteRegKey HKLM "Software\\Google[\s\S]*VELOX_REG_VIEW_BACK[\s\S]*\$\{EndIf\}/);
  assert.ok(!/Policies|ExtensionInstallForcelist/.test(nsh), "never a browser policy");
});

test("the packages' extension lines really write the pointer and remove it again (run on scratch folders)", async () => {
  const { execFileSync } = await import("node:child_process");
  const os = await import("node:os");
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "velox-pointer-"));
  try {
    const dirs = ["a/extensions", "b/extensions", "c/extensions"].map((d) => path.join(tmp, d));
    const at = (s, re) => s.match(re)[0].replace(/^(\s*)for dir in .*; do$/m, `$1for dir in ${dirs.map((d) => `'${d}'`).join(" ")}; do`);
    const install = at(read("apps/desktop/src-tauri/packaging/postinst.sh"), /^VELOX_EXTENSION_ID=[\s\S]*?^done$/m);
    const postrm = read("apps/desktop/src-tauri/packaging/postrm.sh");
    const remove = /^VELOX_EXTENSION_ID=.*$/m.exec(postrm)[0] + "\n" + at(postrm, /^\s*for dir in [\s\S]*?^\s*done$/m);
    fs.mkdirSync(path.join(tmp, "b"), { recursive: true });
    fs.writeFileSync(path.join(tmp, "b", "other.json"), "{}"); // b: another package's folder, kept
    execFileSync("sh", ["-c", install]);
    for (const d of dirs) {
      const f = path.join(d, "keobnkoppcchnbpmmfodidkcedglbhnn.json");
      assert.deepEqual(JSON.parse(fs.readFileSync(f, "utf8")), { external_update_url: "https://clients2.google.com/service/update2/crx" });
      assert.equal(fs.statSync(f).mode & 0o777, 0o644);
      assert.deepEqual(fs.readdirSync(d), ["keobnkoppcchnbpmmfodidkcedglbhnn.json"], "no leftover temporary file");
    }
    execFileSync("sh", ["-c", install]); // installing again (an upgrade) is fine
    execFileSync("sh", ["-c", remove]);
    assert.ok(!fs.existsSync(path.join(tmp, "a")) && !fs.existsSync(path.join(tmp, "c")), "folders it created and left empty are gone");
    assert.deepEqual(fs.readdirSync(path.join(tmp, "b")), ["other.json"], "another package's files stay");
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test("the portable copy gets the extension from the stores, like the installed app (no extension folder in it)", () => {
  const pkg = read("scripts", "package-release.mjs");
  const portable = pkg.slice(pkg.indexOf("// 2. portable"), pkg.indexOf("// 3. the extension on its own"));
  assert.ok(!/extension\/(chromium|firefox)/.test(portable), "no extension folder in the portable zip");
  assert.match(portable, /velox-native-host\$\{exe\}/, "the browser helper is still beside the app");
  // Settings > Browser installs from the stores: both store addresses are compiled into the app.
  const ids = read("apps", "desktop", "src-tauri", "src", "generated", "identity.rs");
  assert.match(ids, /FIREFOX_STORE_URL: Option<&str> = Some\("https:\/\/addons\.mozilla\.org\//);
  assert.match(read("crates", "browser-bridge", "src", "generated", "identity.rs"), /CHROME_STORE_ID: Option<&str> = Some\("keobnkoppcchnbpmmfodidkcedglbhnn"\)/);
});
