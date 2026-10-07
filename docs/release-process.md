# Release process and hardening gate (Phase 12.4)

```
commit -> format -> lint -> unit tests -> integration tests -> security checks -> dependency audit
       -> build -> artifact hash -> sign -> release
```

Implemented by `.github/workflows/ci.yml` (tests) and `build.yml` (installers), on all three operating systems, and, identically, on a developer machine by
`node scripts/security-gate.mjs`.

## CI/CD (GitHub Actions)

**Tests** (`.github/workflows/ci.yml`): every commit pushed to a branch other than `main` (and pull requests from
forks):

* `test` (Windows, Linux, macOS): format, lint, Rust / UI / extension / policy tests.
* `security`: advisories, licences, the full gate, the SBOM.

**Build** (`.github/workflows/build.yml`): no tests (the branch was tested on every commit before it is merged).
Each system runs `bash scripts/build-release.sh --skip-gate`: Windows x64, Linux x64 (Ubuntu 22.04, so the packages
run on older distributions), macOS Apple silicon and macOS Intel. Each `release/` folder (installers, portable zip,
extension zips, SHA256SUMS.txt) is kept 30 days as the artifact `VeloxFetch-<system>-<branch>`.

* **Push to `main`**: every system is built automatically.
* **Try a branch before merging**: GitHub > Actions > Build > **Run workflow**, choose the branch and the systems
  (all, Windows, Linux or macOS), then download the artifacts from the run and test them by hand.
* **Tag `v*`**: the builds, then a GitHub release as a **private draft**. Unsigned installers are never made public:
  check them (`docs/manual-test.md`), sign them, then publish the draft by hand.
  `git tag v0.1.0 && git push origin v0.1.0`

## Building the full release (one command, any system)

```
bash scripts/build-release.sh                 # Windows (Git Bash), macOS, Linux
bash scripts/build-release.sh --quick-gate    # lint + policy + audits instead of the full gate
bash scripts/build-release.sh --skip-install --skip-gate
```

It installs the exact dependencies (`npm ci`), builds the browser extension, the browser helper and the CLI, then
the app and its installer with the system's release config (`tauri.release.conf.json` on Windows,
`tauri.release.macos.conf.json`, `tauri.release.linux.conf.json`: each bundles `velox-native-host` and the
extension), and `scripts/package-release.mjs` collects `release/`:

| System | Files |
|---|---|
| Windows | `VeloxFetch_<v>_x64-setup.exe`, `VeloxFetch-portable.zip` (programs + `extension` folder) |
| macOS | `VeloxFetch_<v>_<arch>.dmg`, `VeloxFetch-portable-macos-<arch>.zip` (the .app) |
| Linux | `.deb`, `.rpm`, `.AppImage`, `VeloxFetch-portable-linux-x64.zip` |
| all | `VeloxFetch-extension-chromium.zip`, `-chrome-store.zip`, `-firefox.zip`, `SHA256SUMS.txt` |

The zips are written by the script itself (no zip tool needed; Unix permissions are kept so the programs stay
executable). The checksums are verified (`sha256sum -c` / `shasum -c`), then the security gate runs; the script
fails when the gate fails.

## The gate (run before every release)

```
node scripts/security-gate.mjs            # everything: fmt, clippy -D warnings, all tests, audits, SBOM, hashes
node scripts/security-gate.mjs --quick    # lint + policy + audits only
node scripts/security-gate.mjs --require-signed   # additionally fail if the installer is not code-signed
```

| Step | Tool | Fails when |
|---|---|---|
| Formatting | `cargo fmt --check` | any file differs from `rustfmt.toml` |
| Lint | `cargo clippy -- -D warnings` | any warning |
| Tests | `cargo test`, UI tests, extension tests | any failure (Rust: unit, integration, property / fuzz-style) |
| Policy | `tests/security/policy.test.mjs` | a TCP listener, a weaker CSP, a plugin permission, a git dependency, a banned crate, an unpinned CI action, a missing lockfile, a hash mismatch, a secret in a log line... |
| Rust advisories | `cargo audit --deny warnings` | any vulnerability, or any warning not listed (with a reason) in `.cargo/audit.toml` |
| Licences, bans, sources | `cargo deny check` | a licence outside `deny.toml`, OpenSSL, an unreviewed Tauri plugin, an updater, a non-crates.io source |
| npm advisories | `npm audit --audit-level=low` (UI and extension) | any advisory |
| Lockfiles | `cargo metadata --locked`, `npm ci --dry-run` | a lockfile is out of date |
| SBOM | `cargo cyclonedx`, `npm sbom` | not producible; output is CycloneDX 1.5 JSON in `release/sbom/` |
| Artifacts | SHA-256 | a file in `release/` differs from `SHA256SUMS.txt`, or is shipped without a checksum |
| Signature | `Get-AuthenticodeSignature` (Windows) | (only with `--require-signed`) not `Valid` |

The report is written to `release/security-gate-report.json`.

## Scheduling

Advisories change daily even when the code does not. The gate runs with the tests on every branch commit; between
commits, run `node scripts/security-gate.mjs` now and then (or `bash scripts/build-release.sh`, which ends with it).

## Signing (wired; active as soon as the owner adds the certificates)

| Platform | What | Secrets (environment `release` for tags, `build` otherwise) |
|---|---|---|
| Windows | Authenticode, SHA-256 with an RFC 3161 timestamp: `velox-native-host.exe` and `velox-cli.exe` by `scripts/build-release.sh` (signtool), `velox-desktop.exe` and the setup by the bundler (`certificateThumbprint`). An EV / OV certificate removes the SmartScreen warning | `WINDOWS_CERT_PFX_BASE64`, `WINDOWS_CERT_PASSWORD` |
| macOS | Developer ID with the hardened runtime (`packaging/macos/Entitlements.plist`: no entitlements): the helper by the script (`codesign --options runtime --timestamp`), the app by the bundler, then notarized and stapled | `APPLE_CERTIFICATE` (base64 .p12), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`; notarization: `APPLE_ID`, `APPLE_PASSWORD` (app-specific password), `APPLE_TEAM_ID` |
| all | detached armored GPG signature `SHA256SUMS.txt.asc` over the checksums, made by the `release` job | `RELEASE_GPG_KEY` (a key without passphrase, used for releases only) |

How it runs: the Build workflow imports the certificate into the runner's store / a temporary keychain only when its secret
exists (otherwise that system's files are unsigned, and the build says so). Locally, the same script signs when
`WINDOWS_CERTIFICATE_THUMBPRINT` or `APPLE_SIGNING_IDENTITY` is set. A tag still produces a **draft** release, checked by hand
(docs/manual-test.md) before it is made public.

## Verifying a download (for users)

1. Compare the installer's SHA-256 with `SHA256SUMS.txt` (`certutil -hashfile <file> SHA256` on Windows, `shasum -a 256 <file>` elsewhere), and get `SHA256SUMS.txt` from a channel you trust (the release page over HTTPS), not from the same place as a mirror.
2. When signatures exist: verify the detached signature over `SHA256SUMS.txt`, and check that the installer's Windows signature says "VeloxFetch" (right-click > Properties > Digital Signatures).

## Updates

There is no in-app updater, on purpose: a bad updater is the most powerful attack on a desktop app. Until one is designed:
users install new versions manually, and `tauri-plugin-updater` is a banned crate. The design that has to exist first: an update manifest signed with an offline key
whose public half is compiled into the app, HTTPS only, monotonic version numbers (no downgrade), verification *before* anything is written or run, and a documented key-rotation plan.

## Dependency policy

* Cargo and npm lockfiles are committed; CI installs with `--locked` / `npm ci`.
* New dependency: read its licence and maintainers, run the gate, and explain any `deny.toml` change in the commit.
* GitHub Actions are pinned by commit hash and updated by a reviewed change (never by a moving tag).
* Also monitor (manually, on release): the Tauri release notes, WebView2 / WebKitGTK updates (they are part of the OS), and **FFmpeg**: VeloxFetch does not bundle it; it uses the user's own copy,
  so the user must keep it updated (see `docs/security.md`, media engine).
