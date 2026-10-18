# Project configuration (`config/veloxfetch.yaml`)

One file holds the values that identify the project to the outside world, so they are changed in one place
instead of being hunted down across several crates, the installers and the publish scripts.

```
config/veloxfetch.yaml      the single source of truth (edit this)
scripts/gen-config.mjs      reads the YAML and writes the generated files (run after editing)
```

## What is in it

| Key | Used for |
|---|---|
| `product` | the product name (update feed, labels that use the bare name) |
| `releases_owner`, `releases_repo` | the **public** download repository; the release page, the update feed (`latest.json`) and the package prefix are all derived from it |
| `chrome_extension_id` | the unpacked / developer-mode extension id (fixed by its manifest key) |
| `chrome_store_id` | the Chrome Web Store item id (empty = not published) |
| `firefox_extension_id` | the Firefox add-on id |
| `native_host_name` | the name the native messaging host is registered under |
| `chrome_store_update_url` | the Chrome Web Store update service the installers point browsers at |

## How it is used — build time, never run time

The values are **compiled into the signed binary and the installers at build time**. The running app never
reads this file (or any file) for them. That is deliberate and is part of the security model:

> These values are what the app *trusts* — where it fetches its own updates, and which browser extensions may
> drive the native messaging host. If the app read them from a file on disk, anyone on the computer (or malware)
> could repoint the update URL at a hostile server, or add a rogue extension id to the trusted list. Keeping them
> compiled in is what protects an install on an unknown machine.

So the config is a convenience for **whoever builds VeloxFetch**, not a knob on an installed copy.

## Changing a value

1. Edit `config/veloxfetch.yaml`.
2. Run `node scripts/gen-config.mjs`. It rewrites the generated files:
   * `crates/browser-bridge/src/generated/identity.rs` — the extension ids and host name;
   * `crates/system-integration/src/generated/identity.rs` — the Chrome Web Store update URL;
   * `apps/desktop/src-tauri/src/generated/identity.rs` — the product name and the update/release addresses.
   Each carries an `@generated` banner and must not be hand-edited.
3. The command also prints the three files that carry a value **literally** and cannot pull in a generated
   file, so update them by hand if the command names them:
   * `apps/desktop/src-tauri/packaging/postinst.sh`, `postrm.sh` — these run on the user's machine at install
     time, where this repository is absent, so they cannot source a generated fragment;
   * `apps/desktop/src-tauri/installer-hooks.nsh` — the Tauri NSIS bundler compiles this hook in its own
     scaffold folder, where a sibling generated file would not be present.
4. Rebuild.

`tests/security/config.test.mjs` runs `node scripts/gen-config.mjs --check` and fails the build (naming the exact
file) if any generated file or any of the three literal-carrying files has drifted from the YAML. The security
policy test still asserts the compiled-in update addresses and store id are exactly the project's own.

## What is *not* here

Everything a user can change on their own machine — download folder, speed limits, the 64-connection cap,
automatic update checks on/off, and so on — is a run-time setting kept in the app's settings database, not in this
file. This file is only the project's fixed identity.
