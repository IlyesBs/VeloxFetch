# Updates (Phase 19)

VeloxFetch is installed on computers its publisher does not control. An updater is the most powerful way into
every one of them: whoever can make the app accept a file can run code on all of those computers. This updater is
designed so that only the owner's offline signing key can do that, and so that a failure leaves the installed
version as it was.

## How it works

```text
fixed HTTPS address (in the build) --> latest.json + latest.json.sig   (64 KiB / 1 KiB at most, no cookies)
    --> Ed25519 signature, strict, over "VeloxFetch update manifest v1\n" + the exact bytes      (update-verify)
        with a public key compiled into the app (apps/desktop/src-tauri/update-keys.txt)
    --> same product, schema 1, version x.y.z newer than this one, released not before the last one seen
    --> the package for this kind of installation (its address in the release folder, its size, its SHA-256)
    --> <app data>/updates/<name>.part: never more than the signed size; resumed with ranges
    --> exact size + SHA-256 --> renamed to <name>                                          ("ready")
    --> the user presses Install: checked again --> checked once more by the installer step (below)
```

| Step | Where | Rule |
|---|---|---|
| Fetch | `download-manager/src/manager/update.rs` | Only the address compiled into the build (`UPDATE_FEED` in `apps/desktop/src-tauri/src/lib.rs`); `https` only, a redirect may not leave `https`; the user's proxy is used; no cookies, no credentials, no extra headers; the user agent is `VeloxFetch/<version>`, the same for every copy |
| Signature | `crates/update-verify` | Ed25519 `verify_strict` (no malleable or small-order signatures); a key id picks the key; any other key, any changed byte, a missing signature: refused. The manifest is not parsed before its signature passed |
| Content | `crates/update-verify` | `deny_unknown_fields`; product `VeloxFetch`; version `x.y.z` (numbers only); each package: a known system and kind, a file name of safe characters with the kind's extension, an address that is exactly `<release folder>/<tag>/<name>`, a size up to 2 GiB, a lower-case SHA-256 |
| No downgrade | `decide` | A version that is not newer is "up to date", never installed |
| No replay | `decide` + setting `update.released` | A manifest older than one already accepted is refused ("stale"), so an attacker cannot hide a fix by serving an old, genuinely signed manifest |
| Download | `update.rs` | Into the app's own data folder; never past the signed size; kept under its final name only after its size and hash matched; anything else in the folder is removed |
| Install | `system-integration/src/update.rs` | Checked again right before use (below) |

### Installing, per system

| Installation | What happens | Why it is safe |
|---|---|---|
| Windows setup (`nsis`) | The verified `setup.exe` is opened (the system's administrator prompt appears), the app closes | The file is held open with **read-only sharing** from before its last hash until the installer has started: nothing can change, rename or delete it in between. When the release is code-signed, Windows also shows the publisher |
| Linux AppImage | The new AppImage is copied next to the running one **while being hashed**, checked, made executable, `fsync`ed, then renamed over the old one in one step. The new copy starts with `--after-update` and waits for the old one to quit | What lands is exactly what was checked; a power cut leaves either the old or the new file, never half of one |
| `.deb` / `.rpm` | Checked, then opened with the system's package installer (it asks for the administrator password) | The system installer installs only what the user confirms |
| macOS `.dmg` | Checked, then opened; the user drags the app as on first install. Gatekeeper checks the signature and notarization | — |
| Portable (zip) | Told about the new version; **Open the release page** opens the fixed release page | Nothing is replaced |

How the installation kind is found: Windows: `uninstall.exe` next to the program (the setup puts it there, the zip
does not). Linux: `$APPIMAGE` (a real file, not a link) for the AppImage; `/usr` + the `velox-fetch` dpkg list for
the `.deb`, else `rpm` for the `.rpm`. macOS: a program inside `*.app/Contents/MacOS/`.

### What the user sees

* **Settings > Updates:** the installed version, the state (up to date, a version available with its size, the
  download's progress, ready, or why a check failed), **Check now**, **Download the update**, **Install and
  restart** / **Open the installer**, **Open the release page**, **Check for updates automatically** (on by
  default), and when the last check happened.
* **Status bar:** "Up to date" (wide windows) or "Update x.y.z available / ready", which opens Settings > Updates.
* The app checks 60 seconds after start, then every 12 hours, when the setting is on. It **never downloads or
  installs on its own**: the user presses the button.

Error texts never pass on anything from the network: each is chosen by a short code (`network`, `signature`,
`malformed`, `stale`, `corrupt`, `disk`, `install`).

## The signing key

* Made by the owner, once: `node scripts/update-key.mjs generate`. It asks for a passphrase twice, at the terminal,
  without echo (at least 16 characters). It refuses a pipe, a file, a variable or an argument. Echo is turned off
  *before* the question is shown (October 2026: it was turned off just after, so a passphrase typed the instant the
  question appeared could show on the screen; found by the publishing kit's terminal test).
* The private key is written **encrypted** (scrypt N=2^17, r=8, p=1 + AES-256-GCM, the key id as associated data) to
  `~/.config/veloxfetch-release/update-signing-key.json`, readable by the owner only (0600). The tool refuses to
  write it inside the project, or over an existing key.
* The public key line is added to `apps/desktop/src-tauri/update-keys.txt`, which is committed and compiled in.
* **Keep the private key offline:** copy the file to two removable drives kept apart, then delete it from the
  computer; plug a drive in only to sign. Without the passphrase the file is useless; without the file, no update
  can ever be signed.
* **CI never signs.** No signing key is a GitHub secret. A compromised CI or GitHub account cannot ship an update.

## Publishing a version that copies already installed can take

1. Make sure the version you are releasing contains the public key (the key must exist **before** the first
   release: a build without a key never updates itself).
2. Raise the version (`Cargo.toml` `[workspace.package] version`, `tauri.conf.json`, `apps/desktop/package.json`);
   it must be higher than every published one.
3. Build every system's packages (`bash scripts/build-release.sh` on each, or the Build workflow). Put the folders
   together.
4. `node scripts/update-key.mjs manifest --version x.y.z --dir release [--dir <other system's folder>]...`
   writes `latest.json` (each package checked against its folder's `SHA256SUMS.txt`).
5. Plug in the key drive: `node scripts/update-key.mjs sign --key <drive>/update-signing-key.json` (asks the
   passphrase; writes `latest.json.sig`; verifies it with `update-keys.txt`).
6. `node scripts/update-key.mjs verify` (re-hashes every package next to the manifest).
   Steps 3 – 6 and the release text are done by one command: `bash scripts/publish-release.sh` (`docs/publishing.md`).
7. Upload the packages, `latest.json` and `latest.json.sig` to the **draft** release `vx.y.z` of the **public**
   download repository `IlyesBs/VeloxFetch-releases` (the source repository is private; the app's feed names the
   public one), check it
   (`docs/manual-test.md` AG), then publish it. GitHub serves `releases/latest/download/latest.json` only for the
   newest **published, non-pre-release** release, so a draft is never offered.

## Key rotation and recovery

| Situation | What to do |
|---|---|
| Planned rotation | Make a new key (`generate --out <new file>`): its public line is added next to the old one. Ship a version signed with the **old** key (it trusts both). Later versions are signed with the new key. Remove the old line once enough users run a version that trusts the new one |
| Key file lost, passphrase lost | Copies already installed can no longer be updated automatically. Make a new key, ship the next version, and tell users (release page, README) to install it by hand once |
| Key stolen (file and passphrase) | Treat as an emergency: make a new key, ship a version that trusts **only** the new key, signed with the old one (the attacker could do the same, so publish it fast and announce it). Revoke by removing the old line in that release |

## Limits (on purpose, documented)

* Same-user malware: another program running as the user can already change the user's files. On Windows the
  setup is protected between the check and the start (read-only sharing); for `.deb` / `.rpm` / `.dmg` the system
  installer shows what it installs and asks for the administrator's password.
* Freeze: an attacker who blocks the release server keeps the app on its version (it says "could not reach the
  update server"). A manifest older than one already seen is refused, so an old one cannot be passed as current.
* Until the code-signing certificates exist, Windows SmartScreen and macOS Gatekeeper warn about the installer
  itself. The update signature does not depend on them.
* Privacy: a check is one anonymous request to GitHub (`docs/PRIVACY.md`); it can be turned off.

## Tests

* `crates/update-verify` (unit): accepted signature; any byte changed; a bare signature without the context; another
  key, even with the trusted key's id; malformed and upper-case signatures; no keys; weak keys; rotation (two keys);
  every package rule (http, other host, other file, query, `..`, wrong extension, wrong system, duplicates, sizes,
  upper-case hash); strict versions; no downgrade; replay refused; exact size and hash of a file; size limits.
* `crates/update-verify/tests/node_interop.rs`: a manifest signed by `scripts/update-key.mjs` (Node) with a throwaway
  key is accepted by the Rust check, a changed or CRLF-converted one is refused, and that key is not trusted by the app.
* `tests/integration/tests/update.rs` (a local release server): found, downloaded, verified, handed over; changed on
  disk after the check: refused; a tampered package never kept; a longer one cut at the signed size; foreign and
  missing signatures; older versions and replayed manifests (remembered across restarts); an interrupted download
  continues and is verified; no cookie or credential sent; the automatic check setting kept; a copy without keys
  sends no request at all.
* `crates/system-integration` (unit): a file replaced only by exactly the checked bytes (mode 0755, nothing left
  behind on failure); packages checked before they are opened; only a plain `https` page opened.
* `tests/security/policy.test.mjs`: no Tauri updater plugin; well-formed keys only; signature before parsing;
  verified before renaming and before installing; the Windows share lock; no `process.env` in the key tool; no
  private key file anywhere in the repository; a fixed feed address; no command takes an address or a path.
* `tests/e2e/desktop-updates.mjs` (the real app, a local server, a throwaway key): a foreign signature refused in
  words; a signed version offered in Settings and the status bar; downloaded, verified, byte for byte in the app's
  folder; install handed over (a dry run); the status bar opens Settings > Updates; the setting kept; requests carry
  no cookie or credential.
* The key tool itself was driven in a pseudo-terminal: generate, manifest, sign, verify; a wrong passphrase, a short
  passphrase and a piped passphrase are refused.
