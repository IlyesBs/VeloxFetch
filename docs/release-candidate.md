# Release candidate 1.0.0 (Phase 21)

The gate a version passes before it is published. Each line says what was checked, how, and the result for 1.0.0
(October 2026). A line marked **owner** can only be done by the project's owner (keys, certificates, machines).

## Gate

| Area | Check | Result |
|---|---|---|
| Version | `1.0.0` in `Cargo.toml`, `tauri.conf.json`, the UI and extension `package.json` / lockfiles, both extension manifests; `x.y.z` only (the updater refuses anything else) | ✅ one version everywhere (policy test "one version everywhere") |
| Engine | unit + integration tests (ranges, segments, retries, slow servers, hostile servers, source changes) | ✅ `cargo test --workspace`: all pass |
| Resume and crash recovery | real-process crash tests (`crash.rs`), segment re-verification (`reverify.rs`), pause / resume across restarts | ✅ |
| Performance | `velox-bench` matrix, `desktop-perf.mjs` (start-up, 60 fps, IPC p95 1 ms) | ✅ `docs/benchmarks.md` |
| UI | 20 + 1 desktop end-to-end suites on the real app (Linux WebKitGTK), vitest | ✅ all pass |
| Languages | French and Arabic on every page, Settings section, dialog and a failed download's details (`desktop-i18n.mjs`); every fixed Rust message translated (`messages.test.ts`) | ✅ |
| Browser integration | `desktop-browser.mjs` (native host, extension link), extension tests | ✅ |
| Security | `node scripts/security-gate.mjs`: rustfmt, clippy `-D warnings`, 44 policy tests, `cargo audit` (0 vulnerabilities, 614 crates), `cargo deny` (advisories, bans, licences, sources), `npm audit` UI + extension (0), lockfiles in sync | ✅ gate passed |
| Unused code | `cargo machete` (no unused dependencies); no `TODO` / `FIXME` / `dbg!` / `todo!` in shipped code; every debug-only switch (`VELOX_*`) is ignored by release builds | ✅ |
| Updates | signed manifest design, tests, e2e (`docs/auto-update.md`) | ✅ code; **owner**: make the signing key before building 1.0.0 |
| Installers | `scripts/build-release.sh` on each system (NSIS, deb, rpm, AppImage, dmg, portable zips, extension zips, `SHA256SUMS.txt`, SBOM) | **owner**: run it (Linux here; Windows and macOS on those machines, or CI once billing works) |
| Code signing | Authenticode (Windows), Developer ID + notarization (macOS), GPG over `SHA256SUMS.txt` | **owner**: certificates not available yet; the release stays a **draft** until then, or is published unsigned knowingly (SmartScreen / Gatekeeper warn) |
| Three systems | Linux: all automatic tests + e2e. Windows, macOS: compile-checked here for the new update code; full tests need CI or those machines | **owner**: `docs/manual-test.md` on Windows and macOS (sections AA–AH) |

## Security review (Phase 21)

Done on top of the per-feature reviews in `docs/security.md` and `docs/threat-model.md`:

* **Attack surface unchanged except updates.** No new listener, no new plugin, no new permission besides the six
  update commands (main window only, no address or path arguments). CSP unchanged. Debug-only test hooks
  (`VELOX_DATA_DIR`, dry runs, the update test feed, the WebView2 debug port) are compiled into debug builds only.
* **Updates (threat 14):** only an Ed25519 signature from the owner's offline key is believed; no downgrade, no
  replay; nothing installs without the user; the package is checked again under a share lock (Windows) or copied
  while hashed (AppImage). Key handling in `docs/auto-update.md`.
* **Dependencies:** 0 known vulnerabilities (Rust and npm); only crates.io; permissive licences; the new crate
  `ed25519-dalek` (BSD-3-Clause, RustCrypto / dalek) is pure Rust, no unsafe C.
* **Tampering ("cracking"):** VeloxFetch is open source (MIT) with nothing to unlock, so the protection that matters
  is that users run *genuine* builds: published checksums (`SHA256SUMS.txt`, its GPG signature when the key exists),
  code signatures (when the certificates exist), and updates that only the owner's key can sign. A modified copy
  from elsewhere cannot get its changes into installed copies through the updater.

## Release steps (owner)

1. **Update key** (once, before building 1.0.0): `node scripts/update-key.mjs generate` in a terminal; commit
   `apps/desktop/src-tauri/update-keys.txt`; copy the private key file to two offline drives.
2. **Build** on each system: `bash scripts/build-release.sh` (Linux here; Windows and macOS on those machines).
3. **Manifest and signature:** `node scripts/update-key.mjs manifest --version 1.0.0 --dir release [--dir ...]`,
   then `sign`, then `verify` (`docs/auto-update.md`).
4. **Draft release** `v1.0.0` on GitHub with the packages, `SHA256SUMS.txt`, `latest.json`, `latest.json.sig`.
5. **Check the draft** (`docs/manual-test.md`), then publish it.
