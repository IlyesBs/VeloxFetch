# VeloxFetch browser extension: privacy policy

The extension only sends links to the VeloxFetch desktop application running on the **same computer**.

* **What is sent, and when:** when you use "Download with VeloxFetch", "Download selected", or (if you leave it on) when the browser starts a
  download: the link, the file name, the page you were on (referrer) and the browser's user agent. If you turn on **Send cookies**, the
  cookies of the site of that link are added, so downloads that need you to be signed in work. Cookies are never sent from private
  windows.
* **Where it goes:** to the VeloxFetch app on your computer, through the browser's native messaging. Nothing is sent to the
  developer or to any server by the extension.
* **What is stored:** two switches (take over downloads, send cookies) in the browser's extension storage. The app keeps the download
  and, until it finishes, the headers needed to resume it; they are deleted when the download completes or is cancelled.
* **Video detection (on; you can turn it off in the extension's popup):** the extension looks at the type and size of the files pages load, to
  find videos. The list of videos found is kept in the browser's memory for the open tab only and forgotten when you leave the
  page or close the tab; nothing is sent anywhere until you click **Download video**, which sends that one video's link (or, on
  YouTube and similar sites, the page's address) to the app like any other download. The app reads video pages with yt-dlp on your
  computer; your cookies are never given to it.
* **Installing the extension:** VeloxFetch's installer only tells Chrome, Edge and Brave the extension's Chrome Web Store id.
  The browser downloads it from the Chrome Web Store itself, as for any extension; VeloxFetch sends nothing to anyone for this.
  To show "Extension installed and working", the app remembers when the extension last reached it (a time, and whether it came
  from the Chrome Web Store), on your computer only; nothing about pages or links.
* **Video tools:** the desktop app downloads yt-dlp, Deno and ffmpeg from each project's official server (GitHub releases of
  yt-dlp and of its ffmpeg builds, dl.deno.land, gyan.dev, ffmpeg.martin-riedl.de) shortly after it starts, if they are missing,
  and replaces yt-dlp when it is two weeks old. These are plain file downloads: nothing about you or your downloads is sent.
* **No tracking:** no analytics, no advertising, no remote code, no reading of pages except the link list of the current page when you press
  "Find downloads on this page".
* **Contact:** the address of the project's issue tracker.

## Updates (desktop app; can be turned off in Settings > Updates)

* **What is sent:** to check for a new version, the app downloads two small public files from the project's GitHub release
  page (`latest.json` and its signature), 60 seconds after it starts and then every 12 hours, and the new version's package
  only when you press **Download the update**. The request names only the product and its version (`VeloxFetch/1.0.0`):
  no cookie, no account, no identifier, nothing about your downloads. GitHub sees your IP address, as for any visit.
* **Where it goes:** GitHub (`github.com/IlyesBs/VeloxFetch-releases` and GitHub's download servers), through your
  proxy if you set one.
* **What is stored:** the time of the last check, the newest release date seen (so an old release cannot be passed off as
  new), your choice for automatic checks, and the downloaded package in the app's data folder (`updates`), until the next one.
* **Turn it off:** Settings > Updates > Check for updates automatically. Nothing is then sent unless you press Check now.

## The desktop app's log and diagnostics

* **What:** a log of what the app does (downloads started and ended, errors, retries, settings changes) and, per download, its
  diagnostics (attempts, HTTP status, error code, speed, a short timeline). Passwords, cookies, sign-in headers, proxy passwords,
  tokens and the values in links (`?token=...`) are removed before anything is written; your home folder is written as `~`.
* **Where:** on your computer only: `logs/velox.log` in the app's data folder (Settings > Diagnostics > Open log folder), at most a few MB
  (older lines are dropped), and the app's database. Nothing is sent to the developer or anyone else; there is no telemetry.
* **Sharing:** only if you choose to, with "Copy diagnostics report" or the Copy button of a download's diagnostics, which put a text
  on your clipboard for you to paste.

## Smart throttle (desktop app, off unless you turn it on)

When it is on, VeloxFetch looks at the **names** of the programs running on your computer every few seconds (the name
the system shows and the file name of the program; its folder only if you listed a program by its full path), to slow or pause downloads while a game or a call
from your list runs. Only the names are read (not what the programs do), they are compared with your list and
forgotten; nothing is stored or sent anywhere.