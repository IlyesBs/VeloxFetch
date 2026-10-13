# Publishing a release to the public (October 2026)

How a version goes from this private source repository to users: one script that builds the production release with
every check, signs the update manifest, and prepares everything to publish by hand on GitHub and the download sites.

## Two repositories

| Repository | Visibility | Holds |
|---|---|---|
| `IlyesBs/VeloxFetch` | **private** | the source code, the tests, the docs (this file) |
| `IlyesBs/VeloxFetch-releases` | **public** | the README and screenshots for users, and the releases (installers, `SHA256SUMS.txt`, `latest.json` + `.sig`) |

The app's update feed, its release page and the packages' addresses are compiled in and name the public repository
(`RELEASE_PAGE`, `UPDATE_FEED`, `UPDATE_PACKAGES` in `apps/desktop/src-tauri/src/lib.rs`; `REPO` in
`scripts/update-key.mjs`; `PUBLIC_REPO` in `scripts/publish-release.sh`). A policy test keeps the four equal: a manifest
listing packages at another address would be refused by every installed copy. The releases of a private repository
cannot be downloaded by users, which is why the public one exists.

## Before the first release (once)

1. **The update key** (`docs/auto-update.md`), in your own terminal:
   `node scripts/update-key.mjs generate`. Commit `apps/desktop/src-tauri/update-keys.txt`. Copy the key file to two
   offline drives. A release built before this could never update itself; the script refuses to start without it.
2. **The public repository:** create `IlyesBs/VeloxFetch-releases` on GitHub (public, no code), with Issues on.

## Each release

On the Linux computer (the passphrase is typed at a terminal; on Windows use Windows Terminal, where Git Bash gives
programs a real terminal; the mintty window that opens by default does not):

```bash
# Windows and macOS: on those computers, `bash scripts/build-release.sh`, then copy their release/ folder here.
bash scripts/publish-release.sh --key /media/<drive>/update-signing-key.json \
     --merge ~/from-windows/release --merge ~/from-mac/release
```

| Step | What | Stops when |
|---|---|---|
| 1. Preflight | one version everywhere; an update key in `update-keys.txt`; the key file present, a key the app trusts, kept outside the repository; no uncommitted change | anything is wrong, before a minute is spent building |
| 2. Build | `scripts/build-release.sh` with the **full** security gate: format, lint, every test, the release kit tests, audits, licences, SBOM, checksums | the build or the gate fails |
| 3. Check | the built app carries the update key (else installed copies could never update) and is a release build (no debug-only switch names inside) | either is not true |
| 4. Stage | `release/publish/v<version>/files`: each system's packages, each checked against **its own** `SHA256SUMS.txt` first (a copy damaged on the way from another computer is refused); unknown names, another version, two different files under one name are refused; extension zips come from the first folder; the SBOM files | any file is not exactly what was built |
| 5. Sign | `latest.json` over every system's package (addresses at the public repository), signed with your passphrase, then verified with the app's key; `SHA256SUMS.txt` rewritten over every file including the manifest and its signature; with `--gpg-key <id>`, a detached GPG signature `SHA256SUMS.txt.asc` too | not signed, or the signature does not verify |
| 6. Final check | every file against `SHA256SUMS.txt`; `BUILD-INFO.txt` (version, commit, date, key id; kept locally) | a mismatch |

Options: `--skip-build` (publish what `release/` already holds), `--skip-install`, `--merge <folder>` (repeat),
`--gpg-key <id>`, `--github-draft` (creates the GitHub release as a **draft** with `gh`; you check it and press
Publish; nothing is ever published, edited or deleted by the script).

### What the kit holds

```
release/publish/v1.0.0/
  files/            everything to attach to the GitHub release
                    installers, portable zips, extension zips, SBOM, SHA256SUMS.txt, latest.json, latest.json.sig
  RELEASE_NOTES.md  the release text: highlights, a downloads table by system, install and check instructions
  repo/README.md    the public repository's front page (from docs/publish/README-public.md)
  repo/screenshots/ the pictures it shows
  LISTINGS.md       texts for download sites, stores and posts (from docs/publish/listings.md)
  BUILD-INFO.txt    for your records; not published
```

### Then, by hand

1. First time: copy `repo/*` into the public repository and push. Later: only when the README or pictures change.
2. GitHub > Releases > Draft a new release: tag `v<version>`, title "VeloxFetch <version>", the text of
   `RELEASE_NOTES.md`, attach every file of `files/`. Keep `latest.json` and `latest.json.sig`: installed copies
   update from them, and GitHub serves `releases/latest/download/latest.json` only for the newest published release.
3. Download sites: `LISTINGS.md` §8 lists where (all free) and what each needs; texts §2–§6 and §10, captions §7.

## The texts and pictures

* `docs/publish/README-public.md`: the public front page. Problem → answer table, every feature, install per system
  (first start on Windows without a paid certificate: More info > Run anyway; macOS: Open Anyway), how to check a
  file's SHA-256, privacy, FAQ, and short French and Arabic sections.
* `docs/publish/release-notes.md`: the release text; `{{VERSION}}`, `{{DOWNLOADS}}` (table by system with sizes) and
  `{{SYSTEMS}}` are filled in by the kit.
* `docs/publish/listings.md`: basics, taglines (50 / 80 / 132 characters), short and long descriptions, feature list,
  what's new, captions, where to publish (GitHub, AlternativeTo, Softpedia, MajorGeeks, Uptodown, FileHorse,
  SourceForge, winget, Scoop, AUR, Microsoft Store, browser stores, Product Hunt, Reddit), posts in English, French
  and Arabic. Every claim is something the app does or a measurement (`docs/benchmarks.md`). The Chrome Web Store does
  not accept extensions that download from YouTube: the extension is described as sending links to the app.
* `docs/publish/screenshots/`: 11 pictures at 1280×800 (the size the browser stores ask for), taken from the real
  app by `tests/e2e/publish-screenshots.mjs`. The app goes through a local proxy, so links read
  `http://releases.example.org/...` (names reserved for examples) instead of this computer's address, and every
  picture is checked not to show `127.0.0.1`. Retake them after a visible change:

```bash
npm --prefix apps/desktop run build
cargo build -p velox-desktop --features velox-desktop/custom-protocol -p velox-integration-tests
node tests/e2e/publish-screenshots.mjs
```

## Without a paid certificate

The installers are not code-signed until there is a certificate (`docs/release-process.md` § Signing). What still
protects users: `SHA256SUMS.txt` on the release page (and its GPG signature with `--gpg-key`, free), updates that only
the owner's offline key can sign, and the README telling users to download only from the public repository. Free
options to look at later: the Microsoft Store (installs without SmartScreen warnings), and code signing programs for
free software (they require the source to be public).

## Tests

`tests/release/publish-kit.test.mjs` (in the full security gate):
* only shipped names, of this version;
* staging: every system's packages, one checksum list, notes and README filled in, every picture copied, an earlier
  kit replaced;
* refusals: a damaged file, a listed file missing, an unknown file, an old version, two different files under one
  name, extensions only, a folder that is not an earlier kit (never emptied);
* preflight: no key yet (says how to make one), an untrusted key, a missing or wrong key file, a key file inside the
  project, versions that disagree;
* the built app: without the key, or a debug build, refused;
* **the whole script** in a throwaway copy of the project: a throwaway key made and its passphrase typed at a
  pseudo-terminal (`tests/release/pty-run.py`), refused without a key and with uncommitted changes, then a Linux
  release plus a Windows folder merged, signed and verified with the app's key; every published file in
  `SHA256SUMS.txt`; no passphrase or private key in any file or in the output.

`tests/security/policy.test.mjs`: the four repository names agree; the script needs a terminal, never passes a
passphrase, only creates drafts, never pushes or tags, always runs the full gate; the kit reads no environment
variable; shell scripts keep Unix line endings (`.gitattributes`).
