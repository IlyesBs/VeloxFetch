# Publishing the browser extension (what "installed with the app, like IDM" needs)

(The app itself, and the texts for every store and download site: `docs/publishing.md`, `docs/publish/listings.md`.)

Chrome, Edge, Brave and Firefox only accept an extension that is **not** from their store when the user loads it by hand
(developer mode). Programs like IDM avoid that because their extension is **in the store**: the installer only tells the browser
"this extension id is wanted" (an *external extension* entry), and the browser shows "A new extension was added: Enable?" on its
next start. The user just clicks Enable (or Remove).

So the work is in two parts:

1. **Publish the extension** (needs your accounts; below).
2. **Wire the app/installer to the published ids** (small, done once the ids exist; see the end).

## 1. Chrome Web Store (also used by Brave and other Chromium browsers)

* Account: https://chrome.google.com/webstore/devconsole , one-time registration fee (5 USD).
* Package: `release/VeloxFetch-extension-chrome-store.zip` (`npm run build` in `apps/browser-extension`, folder `dist/chrome-store`).
  It has no `key` (the store assigns the id).
* Visibility: choose **Unlisted** if you do not want it in search; it is still installable by id, which is all the installer needs.
* Store listing text: short description "Send downloads from your browser to the VeloxFetch download manager." ; category
  *Productivity*; screenshots 1280x800 (take them from the popup and the app; `docs/screenshots` has the app).
* Privacy tab: single purpose = "send download links to the VeloxFetch desktop app"; use `docs/PRIVACY.md` as the policy text
  (host it at any public URL, e.g. the project's repository page).
* Permission justifications (the form asks for each):
  * `nativeMessaging`: the only way to talk to the VeloxFetch desktop app.
  * `contextMenus`: the "Download with VeloxFetch" entry.
  * `downloads`: take over a download the browser just started (can be turned off in the popup).
  * `activeTab`, `scripting`: list the links of the current page when the user presses "Find downloads".
  * `storage`: remember the two popup switches.
  * `cookies` + optional site access: only when "Send cookies" is on, for the site of the link being sent.
  * `notifications`: say whether the link was added.
* After the first upload the dashboard shows the **item id** (32 letters). Send it to be added to the allowlist.

## 2. Microsoft Edge Add-ons

* Account (free): https://partner.microsoft.com/dashboard/microsoftedge . Upload the same `chrome-store` zip. Note the extension id.

## 3. Firefox (AMO)

* Account (free): https://addons.mozilla.org/developers/ . Upload `VeloxFetch-extension-firefox.zip`.
* Choose **"On your own"** (unlisted) for a quick automatic signature, or **listed** for the public directory. The signed `.xpi`
  installs permanently (unlike the temporary add-on used today). The add-on id is already `velox-fetch@velox.app`.

## 4. Wiring (done after the ids exist)

* Add the store ids to the native host allowlist (`crates/browser-bridge`, `apps/desktop/src-tauri/src/bridge.rs`): the manifest
  `allowed_origins` list gets `chrome-extension://<store id>/` (and Edge's id).
* Installer / "Install extension" button: register the external extension per user
  * Windows: `HKCU\Software\Google\Chrome\Extensions\<id>` (Chrome), `HKCU\Software\Microsoft\Edge\Extensions\<id>` (Edge),
    `HKCU\Software\Chromium\Extensions\<id>` (Brave, same key family as its native messaging), value `update_url` =
    `https://clients2.google.com/service/update2/crx` (Edge: `https://edge.microsoft.com/extensionwebstorebase/v1/crx`).
  * Linux/macOS: `External Extensions/<id>.json` with `{"external_update_url": "..."}` in each browser's profile-level folder.
  * Firefox: the signed `.xpi` opened once by the app (`about:addons` install prompt) or a `policies.json` entry.
* Until then the button keeps the "Load unpacked" flow.

**Done for the Chrome Web Store (17 October 2026):** the store id is `keobnkoppcchnbpmmfodidkcedglbhnn`
(`CHROME_STORE_ID`), it is trusted next to the developer id, and the installers point Chrome, Edge and Brave at it
for every user (Linux deb / rpm: `/usr/share/<browser>/extensions/<id>.json`; Windows: `HKLM` keys in the 32-bit view;
macOS: the app writes the per-user `External Extensions` file). Install extension opens the store page instead of
a folder, and the first start opens nothing when the browser adds it by itself. Edge uses the same Chrome Web Store
package through Google's update URL (a real Edge 155 on Linux added it by itself). Firefox: still to do after AMO's
review. Details: `docs/store-extension.md`.
