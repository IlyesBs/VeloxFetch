#!/usr/bin/env bash
# VeloxFetch: build the PRODUCTION release, sign its update manifest and prepare everything to publish by hand
# (docs/publishing.md). Run it yourself, in a terminal: it asks for the update key's passphrase.
#
#   bash scripts/publish-release.sh --key <update key file> [options]
#
#   --key <file>        the encrypted update key (made once by `node scripts/update-key.mjs generate`);
#                       default ~/.config/veloxfetch-release/update-signing-key.json
#   --skip-build        do not build: publish what release/ already holds (built by build-release.sh)
#   --skip-install      passed to build-release.sh (no `npm ci`)
#   --merge <folder>    add another system's release/ folder (copied from the Windows or macOS computer);
#                       repeat it for each one. The update manifest then lists every system's package.
#   --gpg-key <id>      also sign SHA256SUMS.txt with this GPG key (free; SHA256SUMS.txt.asc)
#   --github-draft      create the GitHub release as a DRAFT in the public download repository (needs `gh auth
#                       login`). Never published by this script: you check the draft, then press Publish.
#
# What it does, in order (it stops at the first problem; nothing is uploaded unless --github-draft):
#   1. preflight    one version everywhere, the update key trusted by the app and present, nothing uncommitted
#   2. build        scripts/build-release.sh with the FULL security gate (tests, audits, SBOM, checksums)
#   3. check        the built app is a release build and carries the update key (or it could never update)
#   4. stage        release/publish/v<version>/files: every system's packages, each checked against its own
#                   SHA256SUMS.txt; one SHA256SUMS.txt over all; RELEASE_NOTES.md; repo/ (README + screenshots for
#                   the public repository); LISTINGS.md (texts for the download sites)
#   5. sign         latest.json (the update manifest), signed with your passphrase, then verified
#   6. checksums    verified again, then the steps to publish are printed
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# The public download repository (the source repository stays private). The app's update feed names the same one.
PUBLIC_REPO="IlyesBs/VeloxFetch-releases"

key="$HOME/.config/veloxfetch-release/update-signing-key.json"
build=1
build_args=()
merges=()
gpg_key=""
draft=0
while [ $# -gt 0 ]; do
  case "$1" in
    --key) [ $# -ge 2 ] || { echo "--key needs a file" >&2; exit 2; }; key="$2"; shift ;;
    --skip-build) build=0 ;;
    --skip-install) build_args+=(--skip-install) ;;
    --merge) [ $# -ge 2 ] || { echo "--merge needs a folder" >&2; exit 2; }; merges+=("$2"); shift ;;
    --gpg-key) [ $# -ge 2 ] || { echo "--gpg-key needs a key id" >&2; exit 2; }; gpg_key="$2"; shift ;;
    --github-draft) draft=1 ;;
    -h|--help) sed -n '2,28p' "$0"; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
  shift
done

step() { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
note() { printf '    %s\n' "$*"; }
die() { printf '\033[1;31mSTOPPED:\033[0m %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }
for d in "$HOME/.local/node/bin" "$HOME/.cargo/bin"; do case ":$PATH:" in *":$d:"*) ;; *) [ -d "$d" ] && PATH="$d:$PATH" ;; esac; done
export PATH
have node || die "Node.js is needed (bash scripts/build-release.sh installs it)"
[ -t 0 ] || die "run this in a terminal: the update key's passphrase is typed, never piped"
for m in "${merges[@]+"${merges[@]}"}"; do [ -f "$m/SHA256SUMS.txt" ] || die "$m is not a release folder (no SHA256SUMS.txt)"; done
if [ "$draft" = 1 ]; then
  have gh || die "--github-draft needs the GitHub command line (gh), signed in: gh auth login"
  gh repo view "$PUBLIC_REPO" >/dev/null 2>&1 || die "the public repository $PUBLIC_REPO does not exist or gh cannot see it (create it on GitHub first)"
fi
if [ -n "$gpg_key" ]; then
  have gpg || die "--gpg-key needs gpg"
  gpg --list-secret-keys "$gpg_key" >/dev/null 2>&1 || die "no secret GPG key $gpg_key"
fi

step "1/6 Preflight"
out="$(node scripts/publish-kit.mjs preflight --key "$key")" || die "fix the above, then run again"
printf '%s\n' "$out" | sed '/^VERSION=/d;s/^/    /'
version="$(printf '%s\n' "$out" | sed -n 's/^VERSION=//p')"
key_id="$(printf '%s\n' "$out" | sed -n 's/.*update key \([0-9a-f]*\)$/\1/p')"
commit="$(git rev-parse --short HEAD)"
note "commit $commit, public repository $PUBLIC_REPO"

if [ "$build" = 1 ]; then
  step "2/6 Building the release with the full security gate (this takes a while)"
  bash scripts/build-release.sh ${build_args[@]+"${build_args[@]}"} || die "the build or the security gate failed (see above and release/security-gate-report.json)"
else
  step "2/6 Build skipped (--skip-build): using release/ as it is"
  [ -f release/SHA256SUMS.txt ] || die "release/ is empty: build first (drop --skip-build)"
fi

step "3/6 Checking the built app"
node scripts/publish-kit.mjs check-binary || die "this build cannot be published"

stage="release/publish/v$version"
step "4/6 Preparing $stage"
from_args=(--from release)
for m in "${merges[@]+"${merges[@]}"}"; do from_args+=(--from "$m"); done
node scripts/publish-kit.mjs stage --version "$version" --out "$stage" "${from_args[@]}" || die "staging refused (see above)"
files="$stage/files"

step "5/6 Signing the update manifest (type the update key's passphrase)"
node scripts/update-key.mjs manifest --version "$version" --dir "$files"
node scripts/update-key.mjs sign --key "$key" --manifest "$files/latest.json" || die "not signed"
node scripts/update-key.mjs verify --manifest "$files/latest.json" || die "the signed manifest does not verify"
# The manifest and its signature are published too: list them in the checksums.
(cd "$files" && rm -f SHA256SUMS.txt && for f in *; do
  if have sha256sum; then sha256sum "$f"; else shasum -a 256 "$f"; fi
done > ../SHA256SUMS.tmp && mv ../SHA256SUMS.tmp SHA256SUMS.txt)
if [ -n "$gpg_key" ]; then
  gpg --batch --yes --local-user "$gpg_key" --armor --detach-sign --output "$files/SHA256SUMS.txt.asc" "$files/SHA256SUMS.txt"
  gpg --verify "$files/SHA256SUMS.txt.asc" "$files/SHA256SUMS.txt" 2>/dev/null || die "the GPG signature does not verify"
  note "SHA256SUMS.txt.asc: signed with $gpg_key"
fi

step "6/6 Final check"
if have sha256sum; then (cd "$files" && sha256sum -c --quiet SHA256SUMS.txt); else (cd "$files" && shasum -a 256 -c --quiet SHA256SUMS.txt); fi
printf 'VeloxFetch %s\ncommit %s\nbuilt %s on %s\nupdate key %s\n' "$version" "$commit" "$(date -u +%FT%TZ)" "$(uname -sm)" \
  "$key_id" > "$stage/BUILD-INFO.txt"
note "all files match SHA256SUMS.txt"

if [ "$draft" = 1 ]; then
  step "Creating the DRAFT release v$version in $PUBLIC_REPO"
  gh release create "v$version" --repo "$PUBLIC_REPO" --draft --title "VeloxFetch $version" \
    --notes-file "$stage/RELEASE_NOTES.md" "$files"/*
  note "draft created: check it on GitHub, then press Publish release"
fi

step "Ready: $stage"
ls -la "$files"
cat <<EOF

Publish it (docs/publishing.md):
  1. Public repository https://github.com/$PUBLIC_REPO
     - first time: copy $stage/repo/* (README.md, screenshots/) into it and push
  2. Release v$version: $( [ "$draft" = 1 ] && echo "the draft is there: check it, then press Publish release" || echo "Releases > Draft a new release > tag v$version, title \"VeloxFetch $version\", text from RELEASE_NOTES.md, attach every file of $files" )
     - keep latest.json and latest.json.sig in it: installed copies update from them
  3. Download sites: texts in $stage/LISTINGS.md, pictures in $stage/repo/screenshots
EOF
