# The extension from the Chrome Web Store, installed with the app (v2.0.0)

The VeloxFetch extension is published in the Chrome Web Store as `keobnkoppcchnbpmmfodidkcedglbhnn`. Chrome, Edge and
Brave now get it from there. Before this, the user had to turn on Developer mode, press "Load unpacked" and pick a
folder that VeloxFetch opened.

* **When VeloxFetch is installed**, the installer points the browsers at the store extension (Chromium's documented
  "external extensions"). The browser downloads it from the Chrome Web Store by itself.
* **On the first start**, VeloxFetch connects the default browser and opens the extension's store page in it.
  **Install extension** in Settings › Browser does the same for another browser.
* **No folder and no extensions page open any more** for Chrome, Edge or Brave. Firefox keeps today's way (a temporary
  add-on) until its add-on passes review on addons.mozilla.org.

## What the user sees

| Where | What |
|---|---|
| Right after installing | Linux: the browser adds the extension, switched on, with nothing to click. A real Brave did this in a throwaway profile (below). Windows and macOS: the browser shows its own **New extension added**, and one click on **Enable extension** turns it on. Chrome asks this way for every extension a program points it at. |
| First start of VeloxFetch | The default browser opens on the VeloxFetch page in the Chrome Web Store (**Add to Chrome** / **Add to Brave** / **Get** in Edge). |
| Settings › Browser integration | A **Chrome Web Store** badge (a shield, in the accent colour) on the Chrome, Edge and Brave rows, with the tooltip "The extension comes from the Chrome Web Store and keeps itself up to date". The introduction says the browsers get it from the Chrome Web Store. **Install extension** connects the browser and opens the store page. The steps show the store's: "opened the VeloxFetch extension's page in the Chrome Web Store", then "Press Add to Chrome (Add to Brave in Brave, Get in Edge)… or Enable extension". If no browser could be opened, the page's address is shown with **Copy link**. |
| Firefox row | No badge; the same temporary add-on steps as before. |

Screenshots: `docs/screenshots/settings-browser-store.png` (the app), `store-extension-page.png` (the store page the
app opened in Brave), `store-extension-installed.png` (Brave's extensions page: VeloxFetch **On**, Developer mode off,
"Added by a third-party", Remove extension available).

## Where the pointer goes

The pointer is a small entry saying "extension `keobnkoppcchnbpmmfodidkcedglbhnn`, updated from
`https://clients2.google.com/service/update2/crx`". Only this pointer is written. VeloxFetch never writes a browser
file, a profile, a policy or a local package. The browser fetches the signed extension from its store, and the user
can switch it off or remove it like any other extension.

| System | Who writes it | Where | Removed |
|---|---|---|---|
| Linux (deb / rpm) | `packaging/postinst.sh`, as root, for every user | `/usr/share/google-chrome/extensions/<id>.json` (Chrome), `/usr/share/chromium/extensions/<id>.json` (Chromium, Brave: the Brave binary carries this folder), `/usr/share/microsoft-edge/extensions/<id>.json` (Edge). Content: `{"external_update_url": …}`, mode 644, written to a temporary file and moved into place | `postrm.sh` on remove / purge (not on upgrade). A folder left empty is removed too; another package's files stay. |
| Windows (setup) | `installer-hooks.nsh`, as administrator, for every user | `HKLM\Software\{Google\Chrome, Microsoft\Edge, BraveSoftware\Brave-Browser}\Extensions\<id>`, value `update_url`, in the 32-bit registry view (`Wow6432Node` on 64-bit Windows), where the browsers read it | on uninstall (not during an update) |
| Windows (app) | Install extension, for this user | `HKCU\Software\…\Extensions\<id>` (`store_extension::support`) | Disconnect |
| macOS (app) | first start / Install extension, for this user (a `.dmg` has no install script) | `~/Library/Application Support/<browser>/External Extensions/<id>.json` | Disconnect |
| AppImage | nothing can be written for every user without root | the store page opens: one click on **Add to Brave / Chrome** | n/a |

The id and the places are written once in Rust and checked against the installers by a test
(`tests/security/policy.test.mjs`):

* `CHROME_STORE_ID` in `crates/browser-bridge/src/caller.rs`;
* `LINUX_SYSTEM_FOLDERS` and `WINDOWS_MACHINE_KEYS` in `crates/system-integration/src/store_extension.rs`.

## The store extension talks to the app

* **Trusted callers.** `chromium_ids()` returns the developer id and the store id. Every native-messaging manifest
  allows both origins, and the helper's own allow-list checks both again.
* **Updates bring old registrations up to date.** Someone who connected a browser with an older version has a manifest
  that names only the developer id, so the store extension would get "native messaging host not found". At start,
  `Bridge::refresh_registrations` writes it again (`native_messaging::needs_refresh`):
  * it rewrites only a manifest that names our host and either our own helper or a helper that no longer exists;
  * it leaves alone another copy of VeloxFetch whose helper still exists, another program's file, or anything that is
    not JSON;
  * it runs in release builds, or in a test that gave its own scratch places, and never in a debug build on real
    registrations.

## How it works

* `bridge.rs` `install_extension`, for Chrome, Edge and Brave when a store id is set:
  1. connect the browser (as before);
  2. write the per-user pointer where the system has one (`store_way`: in tests, a scratch home or key);
  3. open `https://chromewebstore.google.com/detail/<id>` in that browser (`browser_launch::open_store_page`, a fixed
     URL and an argument list, no shell);
  4. answer `method: "store"`, `storeUrl`, `folder: ""`. No folder is copied and nothing else is opened.
* `install_extension` for Firefox: the developer folder, as before.
* `auto_install_once` (first start, once per installation, default browser only) calls the same flow.
* `BrowserIntegrationDto.chromeStoreId` tells the window that the store is in use (badge, introduction).
* Texts: `br.introStore`, `br.storeBadge`, `br.storeBadgeHint`, `br.stStore`, `br.stStoreOpenYourself`,
  `br.stStoreAccept`, `br.copyLink`, in all 8 languages.

## Safety

* **Nothing silent and nothing forced.** No enterprise policy (`ExtensionInstallForcelist`): that would mark the browser
  "Managed by your organization" and stop the user removing the extension. Only the documented external-extension
  entry is used; the browser applies its own rules (its prompt on Windows and macOS) and the user can remove the
  extension at any time.
* **Only the store's signed package.** The pointer names Google's update service, and the browser checks the
  package's signature and id. No local `.crx` is shipped, and nothing is loaded from a folder.
* **No new input.** The UI still sends only a browser name from a closed set; the URL, the id and the places are
  constants.
* **No new network traffic from VeloxFetch.** The browser contacts its own store as it does for any extension.

## Tests

* `browser-bridge` unit test: the store id is trusted next to the developer one, and nothing else is.
* `system-integration` (`native_messaging.rs`): an older registration of ours is refreshed. Up-to-date ones, another
  copy's, another program's and broken files are not.
* `tests/security/policy.test.mjs`, two tests:
  * the installers' id, folders and keys match the Rust constants, are removed on uninstall but not on upgrade, and
    never use a policy or a local package;
  * the packages' own lines, run on scratch folders, write the pointer (mode 644, no temporary file left), can run
    twice, and remove it, with the empty folders, while another package's file stays.
* `tests/e2e/desktop-browser.mjs`:
  * an older Edge registration is brought up to date at start, and another program's file is untouched;
  * the store extension's id is accepted by the real helper;
  * the manifest allows both ids;
  * the badges show on Chrome, Edge and Brave but not on Firefox;
  * Install extension gives the store steps, the store address, and no folder or Developer mode;
  * Firefox still gets its folder;
  * on macOS and Windows, the per-user pointer is checked in a scratch home or key.
* `tests/e2e/real-store-extension.mjs` (by hand, internet, Linux with Brave), in a throwaway profile on a hidden
  display:
  * first start with Brave as the default browser opens exactly the store page, never a folder (a stand-in
    `xdg-open` records any attempt) and never the extensions page;
  * Brave adds the extension from the Chrome Web Store by itself, through the packages' pointer, switched on;
  * the store extension pings this test's VeloxFetch through Brave's native messaging, and a link it hands over is
    downloaded intact.

  Without root, the pointer goes into a private copy of the installed Brave: Chromium also reads the `extensions`
  folder beside its own program, and the pointer is written by `postinst.sh`'s own lines.
* NSIS: the hooks compile with makensis 3.11 for x64 and x86 (with an uninstaller).

## Measured on a real computer (17 October 2026, Linux, Brave 1.97.56, 3 runs out of 3)

* **First start.** Brave was opened with exactly `https://chromewebstore.google.com/detail/keobnkoppcchnbpmmfodidkcedglbhnn`.
  No folder was opened and no extension folder was prepared.
* **The pointer.** Brave added VeloxFetch 1.0.0 from the Chrome Web Store within seconds: location 6 (external pointer,
  downloaded), `from_webstore: true`, no disable reasons. Its extensions page shows it **On**, Developer mode off.
* **The store extension and the app.** The ping came back `{"app":"VeloxFetch","protocol":1,"version":"1.0.0"}` through
  Brave → native messaging → helper → app. An 8 MB link it handed over finished byte-exact.

**Still to check by hand on Windows:** run the setup once, then check that
`HKLM\SOFTWARE\WOW6432Node\Google\Chrome\Extensions\keobnkoppcchnbpmmfodidkcedglbhnn` exists and that Chrome shows
"New extension added" (see `docs/manual-test.md` B). No Windows computer was available here.
