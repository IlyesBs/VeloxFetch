# The extension from the Chrome Web Store, installed with the app

The VeloxFetch extension is published in the Chrome Web Store as `keobnkoppcchnbpmmfodidkcedglbhnn`. Chrome, Edge and
Brave now get it from there. Before this, the user had to turn on Developer mode, press "Load unpacked" and pick a
folder that VeloxFetch opened.

* **When VeloxFetch is installed**, the installer points the browsers at the store extension (Chromium's documented
  "external extensions"). The browser downloads it from the Chrome Web Store by itself.
* **On the first start**, VeloxFetch connects the default browser and opens nothing when the installer's pointer is
  there: the browser adds the extension by itself. VeloxFetch knows it has arrived when the extension first reaches
  it, and says so (a green banner, and Settings › Browser shows "Extension installed and working"). Only when it has
  not come after 3 minutes does a banner offer the store page: installing by hand is the fallback, not the way. With
  no pointer (an AppImage) the first start opens the store page, as before. **Install extension** in Settings ›
  Browser connects another browser and opens the store page.
* **No folder and no extensions page open any more** for Chrome, Edge or Brave. Firefox keeps today's way (a temporary
  add-on) until its add-on passes review on addons.mozilla.org.

## What the user sees

| Where | What |
|---|---|
| Right after installing | Linux (deb / rpm): the browser adds the extension, switched on, with nothing to click: real Chrome 155, Edge 155 and Brave 1.97 did, through the pointers the package wrote (below). A browser that is open adds it within a minute. Windows and macOS: the browser shows its own **New extension added**, and one click on **Enable extension** turns it on. Chrome asks this way for every extension a program points it at. |
| First start of VeloxFetch | The default browser is connected; nothing opens. Settings › Browser says "Your browser is adding the extension by itself from the Chrome Web Store: nothing to do". |
| The extension reaches VeloxFetch | A green banner on every screen: "**Browser extension installed** and connected to VeloxFetch: downloads you start in your browser now come here by themselves" (OK, or it goes after 20 s). Settings › Browser: "**Extension installed and working**: it last reached VeloxFetch just now", with the Chrome Web Store badge. Shown once; a restart keeps the green status without the banner. |
| Not there after 3 minutes | A banner: "The browser extension has not reached VeloxFetch yet. Your browser adds it by itself when it starts; if it shows New extension added, press Enable extension. Or install it now:" **Install from the Chrome Web Store** / **Later** (until the next start). The same button is in Settings › Browser while the extension has not reached the app. |
| No pointer (AppImage) | The first start opens the store page in the default browser (**Add to Chrome** / **Add to Brave** / **Get** in Edge). |
| Settings › Browser integration | A **Chrome Web Store** badge (a shield, in the accent colour) on the Chrome, Edge and Brave rows, with the tooltip "The extension comes from the Chrome Web Store and keeps itself up to date". The introduction says the browsers get it from the Chrome Web Store by themselves. **Install extension** connects the browser and opens the store page. The steps show the store's: "opened the VeloxFetch extension's page in the Chrome Web Store", then "Press Add to Chrome (Add to Brave in Brave, Get in Edge)… or Enable extension". If no browser could be opened, the page's address is shown with **Copy link**. |
| Firefox row | No badge; the same temporary add-on steps as before. |

Screenshots: `docs/screenshots/extension-arrived.png` (the green banner, after a real browser added the extension),
`settings-extension-working.png` (Settings: installed and working), `extension-waiting.png` (the banner after the
wait), `settings-browser-store.png` (Install extension's steps), `store-extension-page.png` (the store page),
`store-extension-installed.png` (the browser's extensions page: VeloxFetch **On**, Developer mode off, "Added by a
third-party", Remove extension available).

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
* `auto_install_once` (first start, once per installation, default browser only):
  * `browser_adds_it_itself`: on Linux, the packages' pointer for this browser is in place
    (`store_extension::system_pointer`: Chrome's, Chromium's for Brave, Edge's folder); on Windows and macOS the app
    writes the per-user pointer (`store::offer`). Then it only connects the browser and starts the wait
    (`browser-extension/.awaiting`, the time it started; the window is told: `velox://extension`, `waiting`);
  * otherwise (an AppImage, Firefox) the Install extension flow: the store page, or Firefox's extensions page.
* `ContactLog`: the bridge server calls `Handler::contacted` after every handshake that passed (right token, an
  allowed extension). The log keeps the last contact in memory and in `browser-extension/contact.json`
  (`{"at": <ms>, "fromStore": true}`, nothing else; saved at most every 10 minutes, at once when the source changes),
  ends the wait, and tells the window (`velox://extension`, `contact`, `announce` the first time ever or when it was
  awaited).
* `BrowserIntegrationDto`: `extensionContact`, `extensionAwaitedSince`, `extensionWaitMs` (3 minutes).
* `ExtensionBanner.tsx` (every screen): green "installed and connected" on an announced contact; after the wait,
  the store-page banner (Install from the Chrome Web Store calls Install extension for the default browser; Later
  hides it until the next start). `BrowserSettings.tsx`: the status line above the browsers.
* `BrowserIntegrationDto.chromeStoreId` tells the window that the store is in use (badge, introduction).
* Texts: `br.introStore`, `br.storeBadge`, `br.storeBadgeHint`, `br.stStore`, `br.stStoreOpenYourself`,
  `br.stStoreAccept`, `br.copyLink`, `br.extOk`, `br.extWaiting`, `br.extNone`, `extb.arrived`, `extb.waiting`,
  `extb.install`, `extb.later`, `extb.ok`, in every language.

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
* **A contact proves nothing new and keeps nothing personal.** It is counted only after the link's handshake passed
  (the token and the allow-list, as before); a wrong token or another extension is not a contact. Only the time and
  "from the store" are kept, in the app's own data folder; never a page, a link or a browser profile.

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
* `crates/browser-bridge/tests/link.rs`: each contact of an allowed extension reaches the app once, after the
  handshake; a wrong token or another extension is not a contact.
* `bridge.rs` unit tests: the first contact is announced, saved and remembered after a restart (not announced again);
  the awaited extension ends the wait and is announced; a damaged contact or wait file is ignored.
* `store_extension.rs`: the packages' pointer is looked for in each browser's own folder; a bad id gives nothing.
* `tests/e2e/desktop-extension-wait.mjs` (Linux; a real helper plays the browser): first start with the pointer
  connects the default browser, opens nothing, Settings says the browser adds it; after the wait the banner offers
  the store page and Later hides it; the store extension's contact gives the green banner and Settings "installed
  and working" with the badge, ends the wait, keeps only `at` and `fromStore`; a restart remembers it without the
  banner. Without the pointer (AppImage), the first start opens the store page and does not wait.
* `tests/e2e/real-store-extension.mjs` (by hand, internet, Linux with Chrome, Edge or Brave:
  `VELOX_REAL_BROWSER`, `VELOX_REAL_BROWSER_DIR` for a browser unpacked from its .deb without installing it), in a
  throwaway profile on a hidden display:
  * first start with that browser as the default one connects it and opens nothing (a stand-in launcher and
    `xdg-open` record any attempt); Settings says the browser adds it;
  * the browser, started as a user's would be, adds the extension from the Chrome Web Store by itself, through the
    packages' pointer, switched on;
  * the extension reaches VeloxFetch by itself: the green banner, Settings "installed and working", the wait over;
  * the store extension pings this test's VeloxFetch through the browser's native messaging, and a link it hands
    over is downloaded intact.

  Without root, the pointer goes into a private copy of the installed Brave: Chromium also reads the `extensions`
  folder beside its own program, and the pointer is written by `postinst.sh`'s own lines.
* NSIS: the hooks compile with makensis 3.11 for x64 and x86 (with an uninstaller).

## Measured on a real computer (17 October 2026, Linux)

**The real installation.** VeloxFetch 1.0.0 was installed from its .deb at 14:45:11, while Brave was open (started at
14:44:36). At 14:46:08 Brave added VeloxFetch by itself: its record shows location 6 (an external pointer, downloaded),
`from_webstore: true`, no disable reasons. Nobody clicked anything. (The app of that build then still opened the store
page on its first start, which made it look like a manual step: fixed here.)

**The real test, with the pointers that installation wrote** (`/usr/share/{google-chrome,chromium,microsoft-edge}/
extensions/keobnkoppcchnbpmmfodidkcedglbhnn.json`), each browser in a fresh profile, all checks passed:

| Browser | Added by itself | First contact with the app | Ping and handed-over link |
|---|---|---|---|
| Google Chrome 155.0.8059.84 (unpacked from its .deb) | location 6, from the store, switched on | by itself, a few seconds after | byte-exact |
| Microsoft Edge 155.0.4283.62 (unpacked from its .deb) | location 6, from the store, switched on | by itself | byte-exact |
| Brave 1.97.56 (installed) | location 6, from the store, switched on | by itself | byte-exact |

The first start opened nothing in all three runs (no store page, no folder), and the window showed "Browser extension
installed" once the extension reached it.

**Still to check by hand on Windows:** run the setup once, then check that
`HKLM\SOFTWARE\WOW6432Node\Google\Chrome\Extensions\keobnkoppcchnbpmmfodidkcedglbhnn` exists and that Chrome shows
"New extension added" (see `docs/manual-test.md` B). No Windows computer was available here.
