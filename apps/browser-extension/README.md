# VeloxFetch browser extension

Sends links from Chrome, Edge, Brave (any Chromium browser) and Firefox to the VeloxFetch desktop app.
Manifest V3, no runtime dependencies. One content script, registered at run time: the "Download video" bar, while video detection is on.

## Use

1. Install VeloxFetch and open it once.
2. Build and load the extension (below).
3. In VeloxFetch open **Settings > Browser integration** and press **Connect** for your browser.
4. Right-click a link, image, video or audio and choose **Download with VeloxFetch**, or open the toolbar
   popup and use **Find downloads on this page**.

Popup options:

* **Take over the browser's downloads** (on by default; cookies option is off by default): the browser's own download is cancelled *only after* VeloxFetch has
  accepted the link; if VeloxFetch cannot take it, the browser keeps its download.
* **Send cookies**: for sites that need you to be signed in. The browser asks for access to each site the
  first time. Cookies are never sent from private windows.
* **Detect videos** (on by default; it can be turned off here): a small
  **Download video** bar appears over videos you can download (move the pointer over the video), the toolbar icon shows how
  many were found, and the popup lists them. Click it and VeloxFetch opens its video dialog to choose the quality.
  * Works for video files a page plays (MP4, WebM...), HLS / DASH streams, and video pages on YouTube, Vimeo, Dailymotion,
    X, TikTok, Facebook, Instagram, Reddit and a few more (the app downloads the free programs those sites need, yt-dlp, Deno and ffmpeg, by itself).
  * Services that protect their videos with DRM (Netflix, Shahid, STC tv, Prime Video, Disney+, OSN+...) are recognised: the bar
    says they cannot be downloaded. No tool can download them without breaking the protection, and VeloxFetch does not try.

## Build and load

```bash
cd apps/browser-extension
npm run build          # -> dist/chrome and dist/firefox
npm test               # validation, least-privilege and id-drift checks (Node's test runner)
```

* **Chrome / Edge / Brave:** open `chrome://extensions` (or `edge://extensions`, `brave://extensions`), turn on
  *Developer mode*, choose *Load unpacked* and pick `dist/chrome`. The extension id will be
  `gbcanjaaelioalfdggkhbbnbinmfgdbc` (fixed by the public `key` in the manifest); the native host only accepts
  that id.
* **Published:** the Chrome Web Store item `keobnkoppcchnbpmmfodidkcedglbhnn` (built from `dist/chrome-store`, no
  `key`). The native host trusts it too (`CHROME_STORE_ID`), and the app's installers point Chrome, Edge and Brave at
  it (`docs/store-extension.md`). The developer build above stays for development.
* **Firefox:** open `about:debugging#/runtime/this-firefox`, *Load Temporary Add-on*, pick
  `dist/firefox/manifest.json`. (A permanent install needs the add-on to be signed.) Firefox 140 or newer (Android
  142): the manifest declares what the add-on hands to the app (`data_collection_permissions`: `browsingActivity`,
  `websiteContent`, `authenticationInfo`), which addons.mozilla.org requires and Firefox shows at install.

## Permissions, and why

| Permission | Why |
|---|---|
| `nativeMessaging` | The only way out of the extension: talk to the `velox-native-host` helper. |
| `contextMenus` | The "Download with VeloxFetch" menu entry. |
| `activeTab`, `scripting` | Read the links of the current page when you open the popup and press *Find downloads* (nothing runs on pages otherwise). |
| `downloads` | Optional takeover of the browser's downloads. |
| `storage` | Remember the two popup options. |
| `cookies` (+ optional site access) | Only when *Send cookies* is on, and only for the site of the link you send. |
| `notifications` | Tell you whether the link was added. |
| `webRequest` + access to http(s) sites | *Detect videos*: read the *response headers* of what pages load (type and size) to spot videos, and show the bar on pages. It cannot block or change anything (no `webRequestBlocking`), and nothing leaves the browser except the video you click. Turning detection off removes the bar. |

There is deliberately no `<all_urls>`, `webRequestBlocking`, `tabs` or `history`.

## Layout

```
src/lib/validate.js   pure validation (URLs, file names, cookies, messages); shared with the tests
src/lib/media.js      video detection rules (what is a video, DRM services, video sites, the bar's messages)
src/background.js     context menu, optional takeover, popup requests, video detection, the native host call
src/videobar.js       the "Download video" bar (registered only while detection is on)
src/popup.*           status, settings, page link scan
build.mjs             writes dist/chrome and dist/firefox (Chrome's manifest gets the id-pinning key)
test/                 node --test suites
```
