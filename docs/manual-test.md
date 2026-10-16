# VeloxFetch 1.0.0 - manual test on another PC

Windows 10/11 x64. Not code-signed, so SmartScreen will warn ("More info" > "Run anyway").
Verify the files with `SHA256SUMS.txt` (`certutil -hashfile <file> SHA256`).

| File | What |
|---|---|
| `VeloxFetch_1.0.0_x64-setup.exe` | Installer (for all users of the PC: Windows asks for administrator rights). Installs the app **and** its browser helper `velox-native-host.exe`. |
| `VeloxFetch-portable.zip` | The same without installing (programs + `extension` folder). Extract the whole folder, run `velox-desktop.exe`. Keep everything together. |
| `VeloxFetch-extension-chromium.zip` | Only needed if you do not use the Install button: extension for Chrome, Edge, Brave (extract it first). |
| `VeloxFetch-extension-firefox.zip` | Extension for Firefox (temporary add-on). |

Needs the WebView2 runtime (built into Windows 11; the installer fetches it on Windows 10, needing internet once).
Data: `%APPDATA%\app.velox.fetch\`. Downloads: `Downloads\VeloxFetch\`.

## A. The app (quick regression check of Phases 1-6)
1. Launch; add a direct https link to a large file; progress, speed and ETA move; the file opens correctly.
2. Pause/Resume keeps progress; Cancel deletes the partial file.
3. Queue tab: set "Downloads at the same time" to 1 and add 3 downloads: one runs, two wait. Pause queue / Resume queue.
4. Settings > Scheduler and Settings > When downloads finish (try **Sleep** first) as in the previous checklist.

## B. Browser integration (new)
1. Install / extract VeloxFetch and **start it once**. (Portable zip: keep the `extension` folder next to the programs.)
2. In VeloxFetch open **Settings > Browser integration**. Each browser has its own line: press **Install extension** on the line of the
   browser you use (it is marked "Default browser" only if it is your Windows default; Brave is on the "Brave and other Chromium
   browsers" line). VeloxFetch connects that browser, copies the extension into its own data folder, opens the browser on its extensions
   page and opens the folder. The steps are also shown in the app, with the folder path and a *Copy path* button.
3. In the browser (last step, done by you once): **Chrome / Edge / Brave:** turn on *Developer mode*, click *Load unpacked*, choose the
   folder shown. The extension's ID must read `gbcanjaaelioalfdggkhbbnbinmfgdbc` - a different ID is refused on purpose.
   **Firefox:** *Load Temporary Add-on*, pick `manifest.json` in the folder (Firefox forgets it when it closes).
   Later, *Show steps* on a connected line shows these steps again; *Disconnect* removes the connection.
4. Click the extension's toolbar icon: it should say **Connected to VeloxFetch 1.0.0**.
   If it says the browser is not connected, press Disconnect then Install extension again in VeloxFetch.
5. Right-click a download link (a `.zip`, `.iso`, `.pdf`, ...) > **Download with VeloxFetch**. A notification says it was added, and it
   appears in VeloxFetch and completes. Also try right-clicking an image and a video.
6. Toolbar icon > **Find downloads on this page** on a page with several file links: pick some > **Download selected**.
7. **Sites that need a login:** sign in to such a site in the browser, tick **Send cookies** in the popup, right-click a protected file
   link. The browser asks for permission for that site (allow); the download should succeed instead of saving a login page.
8. **Automatic detection with confirmation (on by default):** start a normal download in the browser (e.g. an Ubuntu .iso link).
   The browser's own download disappears at once (no "Save as" window from the browser) and VeloxFetch comes to the front with a
   popup: file name (editable), server, address, size, type, resume support, save folder. **Start download** begins it,
   **Download later** only adds it to the list, **Cancel** downloads nothing. Several at once are answered one after the other.
   Settings > Browser integration > "Ask me before a download from the browser starts" turns the popup off (then it starts at once).
   Untick "Take over the browser's downloads" in the extension popup to let the browser download by itself again.
   **After updating the extension, remove it and load it again** on the extensions page.
9. **Closed app:** quit VeloxFetch completely, then right-click a link > Download with VeloxFetch. VeloxFetch should start by itself
   and take the download (this can take a few seconds). (The popup's status check never starts the app.)
10. **Private/incognito window:** allow the extension in private windows, hand over a link from one: it works, but the site's cookies are
    never sent.
11. Settings > Browser integration > **Disconnect**: the extension now reports the browser is not connected.
12. Uninstall (Settings > Apps): the app and the helper are removed. (If you connected a browser, press Disconnect *before*
    uninstalling; otherwise the browser keeps a harmless registration pointing at a missing file.)

## What to report
Windows and browser versions; the step number; what you saw versus what you expected; a screenshot of the popup or the message shown.


## C. Batch downloads and clipboard (Phase 8)
1. Downloads page > **Several...**. **List of links:** paste a few links (one per line, lines starting with `#` are ignored, repeat one,
   add one junk line). The preview says how many will be added and how many are repeated / already in your downloads / not valid.
   **Add N downloads**. **Import a .txt file...** loads a links file into the box.
2. **Pattern** tab: `https://<a server that has them>/file_[001-100].zip` shows 100 links (zero padded); `[a-e]` counts letters.
   Try `[1-1001]`, or three ranges of 100: refused with a message (the limit is 1000 links). Try the sub-folder `..\..\x`: refused.
3. Settings > **Clipboard detection**: off by default. Turn it on, then copy (from anywhere, VeloxFetch not in front) a link that ends in
   `.zip`, `.iso`, `.exe`, `.pdf`, `.mp4`...: a notice appears in VeloxFetch (the taskbar button flashes). **Download...** opens the usual
   confirmation (details, Start / Cancel). Several such links (one per line) offer the batch dialog.
   Copy ordinary text, a password, or a normal web page link: nothing happens. A link already in your downloads is not offered again.
   Whatever was on the clipboard when you switched it on is not reported.

## D. Video streams: HLS / DASH (Phase 9)
1. Downloads page > **Stream...** (or paste a link ending in `.m3u8` / `.mpd` in the address bar and press Add: it opens the same dialog).
   Paste the link of a stream you may download (e.g. a public test stream), press **Look**. It lists the qualities (best first), the audio
   tracks and the subtitles, and nothing is downloaded yet. Pick a quality, tick a subtitle, press **Download**.
2. A card appears under **Video streams** with the pieces done and a percentage. When it says Completed, the file(s) are in your VeloxFetch
   download folder: with ffmpeg installed (or `ffmpeg.exe` next to VeloxFetch) video and audio are joined into one `.mp4`; without it
   you get `name.ts` (or `.mp4`) plus `name.audio.m4a`, and a note says why. Subtitles are separate `.vtt` files. Play the result.
3. **Cancel** a running one: it stops within a second or two, the card says Cancelled and no half file or hidden `.velox-media-*` folder remains.
4. A stream that is encrypted, uses DRM or is live (e.g. a TV channel) is refused with a clear reason, and nothing is written.
5. Copy a `.m3u8` link with Clipboard detection on (Settings): the notice offers **Download...**, which opens the stream dialog.
6. Ordinary downloads still work as before (add any `.zip`). Stream jobs are not remembered after a restart (the finished files are).

## E. Filesystem security (Phase 10)
1. Downloads > Add: try a **file name** of `..`, `CON`, `a:b`, or one ending in a dot: each is refused with a reason and nothing is created.
   A **sub-folder** of `..\x`, `C:\x` or `/etc` is refused too. A normal one such as `movies/2026` works and is created inside the download folder.
2. Download a file whose link ends in `..%2Fevil.bin` (any test server) or `CON.txt`: it is saved as `evil.bin` / `_CON.txt`
   directly in the download folder, never above it.
3. Windows, optional: in the download folder run `mklink /J movies C:\somewhere-else`, then download into sub-folder `movies`.
   It must fail with "path escapes the download folder" and nothing may appear in `C:\somewhere-else`.
4. Ordinary downloads (any `.zip`), pause / resume, and streams (section D) behave as before.

## F. Secrets and credentials (Phase 11)
1. Settings > Browser integration shows "Cookies and sign-in details ... are kept encrypted on this PC". (If it shows the yellow "Secure storage is not available"
   note instead, tell me which OS: cookies then live in memory only.)
2. Downloads > Add a link with a password in it, e.g. `https://user:secret@your-test-server/file.zip` (a server with Basic auth). It downloads, and the list shows the link
   **without** `user:secret@`. Pause it, close and reopen VeloxFetch, resume: it still authenticates.
3. Windows: `Control Panel > Credential Manager > Windows Credentials` has one entry `database-key-v1.app.velox.fetch` (created on first start). Do not delete it while downloads are paused,
   or their cookies become unreadable (they are then simply not sent).
4. Optional check that nothing is readable: open `%APPDATA%\app.velox.fetch\velox.db` in a hex / text viewer while a browser download with cookies is paused: no cookie text, no `Authorization`.
5. Ordinary downloads, browser handoff (section B), batches, streams and the filesystem checks (sections A-E) behave as before.

## G. Security hardening (Phase 12)
1. **Mark of the Web (Windows).** Download any file (e.g. a `.zip`), then in PowerShell: `Get-Content <file> -Stream Zone.Identifier`. It shows `ZoneId=3` and a `HostUrl=` of the
   address **without** its `?query`. Opening a downloaded `.exe` now shows the usual "downloaded from the internet" warning (that is intended).
2. **No local probing.** Add a link to a site you control that redirects to `http://127.0.0.1:PORT/` or to `http://169.254.169.254/`: the download fails with
   "refused a redirect from a public address to an address on this computer or network". A link you type that points at your own PC or NAS still works.
3. **DLL search.** Put any file named `version.dll` (or another DLL) in the folder VeloxFetch is started from (e.g. Downloads): it must not be loaded. (Only for people who know how to check this in Process Explorer.)
4. Everything from sections A-F behaves as before. If Windows warns that the installer comes from an unknown publisher, that is expected: it is not code-signed yet (see `docs/release-process.md`).
5. Installer: the project's Tauri configuration now requests a **per-machine** install (administrator prompt, installs under Program Files). Check that the app starts, that `velox-native-host.exe` sits next to it, and that Settings > Browser integration > Connect still works after such an install.

## H. Crashes and hostile servers (Phase 13)
The automated suites kill real processes and run hostile servers (see `docs/testing.md`); two things are worth seeing once by hand.
1. **Kill during a download.** Start a large download, then end `velox-desktop.exe` in Task Manager (End task) while it is running. Start VeloxFetch again: the download is listed as **Paused** with
   its progress, and Resume finishes it. Open the finished file (or compare its SHA-256 with the publisher's): it must be intact.
2. **Not enough disk space.** Add a link to a file larger than the free space of the download drive: it fails at once with "not enough free disk space", and no huge empty file is left in the folder.
3. Everything from sections A-G behaves as before.

## I. Download folders and sorting
The system's own folder dialog cannot be driven by the automated tests, so look at it once by hand (on each OS you ship for).
1. **Settings > Downloads folder > Change...** opens the normal "choose a folder" dialog of the system, in front of VeloxFetch. Choose a folder in Documents: it is shown, and a new download lands there.
   "Use the usual folder" brings the default back. Closing the dialog without choosing changes nothing.
2. Try to choose `C:\Windows`, `C:\Program Files`, a drive root, or your Startup folder (`shell:startup`): each is refused with "that folder is not allowed", and nothing changes. (macOS / Linux: `/System`, `/usr`, `/etc`, `~/.ssh`, `~/.config/autostart`.)
3. **Sorting**: with "Sort new downloads into folders by type" on, download an `.mp3`, a `.pdf`, a `.zip`, an `.exe` and a video: they land in `Audio`, `Documents`, `Archives`, `Programs` and `Video` inside the download folder. Rename a folder in Settings and download again.
4. **Confirmation popup** (browser download): "Save in" shows the sorted folder; **Change...** opens the dialog and the file is saved exactly in the folder you pick. The Add dialog (Options) has the same button.
5. Plug a drive, choose a folder on it, remove the drive, restart VeloxFetch: it starts, using the usual folder until the drive is back.

## J. Everyday features (checked by hand once per OS)
The automated tests run "Show in folder" / "Open" as a dry run and never use a real proxy on the internet.
1. **Show in folder / Open.** Finish a `.pdf` and a `.exe`. The PDF's **Open** opens it in the usual reader; **Show in folder** opens the file manager
   with the file selected (Windows Explorer; macOS Finder; Linux: the file manager, or at least the folder). The `.exe` has no **Open**: only
   Show in folder. The same buttons work from History; a file you deleted by hand shows "moved or deleted".
2. **Sign in.** Download from a site that needs a password (or any HTTP Basic test URL): the card says the server asks for a user name and
   password and offers **Sign in...**; with the right one it finishes.
3. **Proxy.** Settings > Network > Manual with a real proxy (e.g. a local Squid or a SOCKS5 tunnel `ssh -D 1080`): new downloads go through
   it; a wrong password gives a clear message; back to System, downloads connect as before.
4. **Drop and paste.** Drag a link from a browser page onto the window: it lands in the link bar. Copy two links and press Ctrl+V (Cmd+V on
   macOS) with nothing selected: "Add several" opens with both.
5. **Same name twice.** Download the same file twice: the second is saved as `name (1).ext`, the first is untouched.
6. Everything from sections A-I behaves as before.

## K. Diagnostics (checked by hand once per OS)
1. Download a file, then a broken link (any URL that answers 404). Open each one's details: the **Diagnostics** card shows the HTTP
   status (206 / 404), the error code (`http_404`), attempts, retries, connections, speed and a timeline. **Copy** puts a text report
   on the clipboard.
2. Settings > **Diagnostics**: the recent log shows `download ended ...` lines; **Problems** keeps warnings and errors; **Detailed
   logging** adds engine lines and is still on after a restart; **Open log folder** opens the file manager on `velox.log`
   (Windows: `%APPDATA%\app.velox.fetch\logs`; macOS: `~/Library/Application Support/app.velox.fetch/logs`; Linux:
   `~/.local/share/app.velox.fetch/logs`). On Linux / macOS `ls -l` shows `-rw-------`.
3. Download from a site that needs a password, with a cookie, and from a link with `?token=...`: search the log file and a copied
   report for the password, the cookie and the token value: none of them is there.

## L. Videos from the browser and download speed (checked by hand once per OS)
1. Nothing to install: the Windows setup's details show "Downloading the video tools..." with yt-dlp / Deno / ffmpeg percentages,
   then "The video tools are ready"; `C:\Program Files\VeloxFetch	ools` holds the three programs. Settings > Browser & clipboard >
   Video tools says "Ready" at once. Install once with the internet off: setup still finishes; the app downloads them at start
   (percentages in Video tools). Close the app in the middle and open it again: the percentage continues, it does not restart at 0.
   Uninstall: the tools folder is gone.
2. Reload the browser extension (it asks for site access when installed). **Detect videos** is on in its popup.
3. Open a YouTube video: move the pointer over it, a **Download video** bar appears at its top-right corner. Click it: VeloxFetch
   opens "Download this video" with the qualities (1080p, 720p... and Audio only) and the **File type** (MP4 chosen). Pick one, Download:
   a `.mp4` that plays in the system player appears in Video. Again with MKV, WebM, and with Audio only as MP3 / M4A / Opus:
   each file has that extension and plays.
   In the Videos list while it downloads: the speed and time left show, the dashboard's speed / active / network cards move;
   **Watch now** plays the part downloaded so far (the picture; the sound is joined at the end); **Pause** stops it (status Paused),
   **Resume** continues from the same percentage; **Cancel** removes the pieces. When finished: **Play**, **Open**, **Show in folder**,
   and the video is in History. Unplug the network for a moment while it downloads: it says "Trying again (2 of 3)" and continues.
4. A page that plays an MP4 file or an HLS stream: the toolbar icon shows a number, the bar offers the video(s).
5. Netflix / Shahid / STC tv: the bar says the site protects its videos with DRM; nothing is downloaded.
6. Without the internet, step 3 says the tools could not be downloaded; with it back, "Get them now" fixes it. Turning **Detect videos** off removes the bar after a page reload.
7. A large file from a fast server: the details' Diagnostics card shows up to 64 connections ("at most"); Settings > queue offers up
   to 64 connections per download.

## M. Smart throttle and Live preview (checked by hand once per OS)
1. Settings > Speed > Smart throttle: turn it on, add a program you can start (e.g. `notepad` on Windows, `TextEdit` on
   macOS, `gedit` on Linux), Save list. Start a large download, then start that program: within ~3 s the status line says
   it is running and the speed drops to the chosen share; close it: full speed again within ~3 s.
2. Download a video (MP4 / WebM) from a fast site, open its details, press **Watch while downloading**: it plays after the
   first megabytes; seeking ahead waits for the data and continues. Download a web page or a program: no Preview card.
3. Automatic mode (on by default with Smart throttle): start a large download, then watch a video or download something in
   the browser: within ~6 s the status says "Reducing speed — active browsing detected" and the graph's dashed limit line
   appears; stop: a few seconds later "Bandwidth optimized — speeding back up", then "Full speed — network idle". Leave the
   computer alone for 2+ minutes: "Full speed — computer idle". The speed never drops below "Downloads always keep at least".
   Manual's Aggressive / Balanced / Performance keep about 35 / 50 / 75 % of the line, all the time.
3b. Three levels (issue #46, once per OS, on a real line): start a large download, then play a 4K YouTube video.
   **Fast**: the video struggles (low quality / buffering), the download keeps full speed. **Smart**: within about a
   minute the status says "Checking whether another app needs the connection", then "Reducing speed"; the video plays
   in high quality; without touching the mouse for 3+ minutes it stays so; stop the video: full speed again within
   ~30 s. With nothing else running, the status shows a short check now and then and goes back to full speed.
   **Manual**: Aggressive / Balanced / Performance each cap the download at once and keep it. Choose the **Fast**
   download profile: the card shows Fast with the levels greyed out, "Fast profile · full speed, never slows down".
4. Steadiness (the October 2026 fixes): with Smart throttle on and nothing else using the network, a large download stays
   at full speed ("Full speed — nothing else is using the connection"), it is not slowed at start. Start a second download
   in the browser: within ~6 s the speed visibly drops to about half; stop it: full speed again within about 30 s, the
   status never sticks on "browsing". In Manual, click Aggressive / Balanced / Performance and the switch quickly
   several times: the download never shows 0 B/s, the switch never flicks back, the gauge's centre stays in the middle and
   every name is written in full. Choosing one of Manual's levels from another level switches to Manual.
4b. Gaming policy (once per OS): Settings > Speed > Smart throttle > "While a listed program runs".
   * *Pause downloads*: with two large downloads running, pause one yourself, then start the listed program: within ~3 s
     the other pauses (its progress stays), the card says "… is running · downloads paused until it closes", the Queue
     view says "Paused while a game or call runs · 1 will resume when it closes"; add a new download: it does not start.
     Close the program: within ~6 s the download it paused resumes from where it was, the new one starts, yours stays
     paused. Repeat, and quit VeloxFetch during the game: at the next launch nothing stays held.
   * *Limit downloads to a low speed* (e.g. 256 KB/s): with the program running the speed stays near it, nothing pauses;
     change your own maximum speed meanwhile: it is still set after the program closes.
   * *Give way by the chosen mode* (default): the same as step 1.
   * Close and reopen the program quickly: the policy does not flip off in between.
   * *Pause* with a video page or stream downloading: it pauses with the game and resumes after it.
   * List a program by its full path (e.g. `C:\Games\x\game.exe`, `/opt/game/run`): only that program triggers it,
     not another one with the same file name elsewhere.
   * Linux: open the details of a downloading MP4 / MP3 and press **Watch while downloading**: it plays (it did not
     before October 2026), seeking ahead waits for the data.
5. Speed without Smart throttle: a large download from a server that limits connections (many mirrors do) keeps a steady
   speed and connection count, instead of swinging between full and nearly nothing every ~15 s.

## N. Fresh computer (once per release, on a PC without developer tools)
1. Install on a Windows PC that never had Visual Studio or the "Visual C++ Redistributable". Connect Brave / Chrome in Settings >
   Browser integration, install the extension, download a file from a web page: it reaches VeloxFetch (no "helper could not start").
2. First start right after the installer: the window appears within ~1 s and is drawn (no black window). With MSI Afterburner /
   RivaTuner running, a crash of the web view brings the window back by itself.
3. Start VeloxFetch a second time from the Start menu: the running window comes to the front, no second copy appears.
4. The Disk tile shows the free space right after start, before any download.
5. Open a page with a video: the **Download video** bar appears over it without changing any setting.

## O. On the desktop: tray, start at login, notifications (Phases 16-18, once per OS)
1. Settings > Appearance > **On the desktop**: the three switches are there (close to tray and notifications on, start at login off).
2. Close the window: it disappears, the tray icon stays (Windows notification area, macOS menu bar, Linux panel), one notification says
   VeloxFetch is still running. A download sent from the browser still arrives. The tray menu opens the window, pauses / resumes all, quits.
3. Linux desktop without a tray (GNOME without the AppIndicator extension): the switch is greyed with an explanation, closing quits.
4. Turn on **Start VeloxFetch when I log in**, log out and in: VeloxFetch is in the tray, no window. Windows: Task Manager > Startup apps
   lists it; Linux: `~/.config/autostart/app.velox.fetch.desktop`; macOS: `~/Library/LaunchAgents/app.velox.fetch.plist`. Turn it off: gone.
5. Start VeloxFetch again from the menu / dock while it is in the tray: the window comes back, no second copy.
6. With the window hidden or behind another one, let a download finish and make one fail: one notification each, with the file name.
   With the window in front: none. Change the language: the tray menu and the notifications follow.
7. Windows: `powercfg /requests` lists VeloxFetch while downloading (keep awake); macOS: notifications and the menu bar icon show.
8. Signed builds: Windows file properties > Digital signatures (app, helper, setup); macOS `spctl -a -vv VeloxFetch.app` says
   "Notarized Developer ID".

## P. Smart Download Queue (once per OS)
1. Queue page: the controls card is unchanged; one **Main queue** tab; downloads keep working as before (no rule, "Download later"
   still waits for Start).
2. **New queue > Night downloads**, set the window to start in a few minutes: the header says "Scheduled · starts …", the rules show as
   chips, the timeline shows the window. Add a download to it (Add dialog > Queue): its row says *Scheduled*. When the window opens it
   downloads; when it ends it pauses (row: *Scheduled*), and goes on at the next opening.
3. **Start** on a closed queue runs it now until it is empty ("Started by hand"); **Pause** pauses its running downloads; Start brings them back.
4. A row's ⋯ > **Start at…** > "In 1 hour": *Scheduled*, a dot on the timeline, the toast says when. ⋯ > **Start now**: it starts at once.
5. Downloads at the same time = 1, three waiting downloads: the arrows move them; the one at the top gets the next slot. A **High**
   download added while a Low one runs starts at once (one extra slot); a Normal one waits.
6. **When I'm away** queue: its downloads wait ("Waiting until the computer is idle"); leave the mouse 2 minutes: they start; move it:
   they pause about 30 s later. Where the system does not report idle time, the editor says the rule is not applied.
7. **Wi-Fi only** queue on a cable: "Waiting for the right connection"; switch to Wi-Fi: it starts. Unplug the network: the header says
   "Waiting for the network" and nothing new starts; plug it back: it goes on. (macOS: online / offline only.)
8. **Light on the connection** queue: start a big download in a browser; after a few seconds the queue pauses ("Paused while the connection
   is busy"), and carries on a little after the browser download ends.
9. A queue with **Big files** over 100 MB: add a 1 GB file to the main queue: it moves to that queue once its size is known (and pauses if
   the queue is closed). Move it back with ⋯ > Move to Main queue: it stays there.
10. Smart Throttle on, browse: *Low* downloads wait ("Waiting while you use the connection"), nothing running stops.
11. **Clear completed** removes the queue's finished downloads (the files stay). **Delete queue**: its downloads move to the main queue.
12. Restart the app: queues, rules, order, start times and the open tab are kept. Switch to Arabic: the page is mirrored, the timeline
    still runs left to right.

## Q. Download Intelligence (once per OS)
1. Add dialog (options) > paste an `https` link to a big file > **Check link**. The panel shows the file, size,
   *Supported*, *Parallel: starts with N connections…*, *Storage: OK (… free)* and *Secure connection (HTTPS)*.
   Warnings show "None". Nothing appears in the list, and nothing appears in the download folder.
2. "Why?" lists what the server did (one byte for one byte, version tag, size).
3. An `http` link: a warning that it is not secure, health "Read the warnings".
4. A link that needs a password (`401`): "The server asks for a user name and password", health "Likely to fail as it is".
5. A link that is already in the list: "Already have: In your list: … · same link". The same file name from another
   site: no "Already have".
6. A signed cloud link (S3 / CloudFront with an expiry): "This link looks temporary and expires …". The address in
   the browser confirmation dialog shows `[redacted]` instead of the signature.
7. A file larger than the free space (or ≥ 4 GB to a FAT32 USB stick): the storage problem is shown, and Start still
   works (the engine refuses it with its usual message).
8. A server that does not answer: "The link could not be checked. You can still download it." Edit the link while
   "Checking…" is shown: the check stops.
9. From the browser: the confirmation dialog shows the panel under the usual rows. Answer before the check ends: the
   dialog closes at once.
10. Switch to French and Arabic: all texts are translated, and the panel is mirrored in Arabic.

## R. Download Doctor (once per OS)
1. A link that answers 404: the detail view shows "The file is not on the server (HTTP 404)", *Needs you*, "The
   problem is the link itself", "Retrying will not help", and "Get a fresh link…". The timeline shows a `Doctor:`
   line before the failure.
2. A server answering 503 (or 429 with `Retry-After`): while the app retries, an amber notice counts down
   ("Retrying on its own in …"). Once it gives up: "Server temporarily unavailable", "on the server's side, not your
   connection", "Retry later…", and a *Retry now* button.
3. Unplug the network, start a download: "This computer is offline", "Check the internet connection…". Plug it back
   in and press Retry: it completes, and the panel goes away.
4. A download larger than the free space: "The destination disk is full", the needed and free sizes, "free about X",
   and an **Open download folder** button.
5. A wrong expected checksum: "The downloaded file is not the one expected", *Critical*, **Restart safely**. Retry
   once more: "Retrying will not help" and "The previous attempt failed the same way". The file never shows as
   completed.
6. A site with an invalid certificate (e.g. `https://expired.badssl.com/`): "The secure connection could not be set
   up". The diagnostics show 0 retries.
7. A browser download whose session expires mid-way (sign out on the site, then Retry): "Your sign-in or session
   has likely expired" and "Start the download again from the page…".
8. French and Arabic: every line is translated, and the panel is mirrored in Arabic.

## S. Smoothness and start-up (once per OS, on a high-refresh screen if there is one)
1. Start VeloxFetch: the window appears with the interface drawn almost at once, in the theme's colours (light theme: no
   dark frame first). Linux may show the empty window in the theme's colour for about a third of a second.
2. Start four or more large downloads, watch the dashboard and scroll it up and down: no stutter. Linux: about 60 fps (the
   web view's own ceiling there, whatever the screen); Windows / macOS: the screen's rate.
3. Linux: cards and panels have no shadows; the selected tab, the current page in the side menu, the chosen Smart throttle
   mode and a focused field still show a clear ring. Switch the theme and turn Smart throttle on (amber): the soft light in
   the background follows within half a second.

## T. Steady speed and a smooth Smart throttle (once per OS, on a real internet connection)
1. Download a large file (1 GB test file) with nothing else on the line: the speed shown stays near the line's speed
   (within about a sixth either way), never drops to a few hundred KB/s for seconds. Details > Timeline: few or no
   "failed" lines.
2. Same from a server that allows only a couple of connections (a public speed-test file host): a steady speed on 1–2
   connections; a refused part waits a moment, the others keep going; no long stretch at 0.
3. Turn Smart throttle on (automatic mode on, nobody else using the line): the window does not flash or flicker (once
   amber, it stays still), the status says full speed and stays so for a minute; downloads keep their speed.
4. Start a listed program (or add a running one to the list): within ~3 s downloads drop to the share, smoothly, with no
   stop; the speed stays near the cap; the connection count drops to a few. Play or run a speed test meanwhile: no
   bursts of lag every second.
5. Close the program: after two scans (~6 s) downloads climb back to full speed in a few seconds, no burst far above the
   line's speed.
6. With the program running, switch modes on the card every few seconds: the download never stops, the window never
   flashes.

## U. Smart Duplicate Detection (once per OS)
1. Download a small document. Paste the same link and press Add: "You already downloaded this file" (or "appears to
   have been downloaded already"), with Open file, Show in folder, View, Download again. Open works.
2. Download again: a second copy `name (1).ext`; the first file is untouched.
3. Paste the link with `#something` at the end, or the host in capitals: same prompt. Change a query value (`?v=2`):
   no prompt.
4. Start a large download, paste its link: "This file is already downloading" (View / Download another copy). Pause it,
   paste again: "paused", Resume it resumes the same download (no new one in the list).
5. Delete the finished file in the file manager, paste its link: "the file is no longer in its folder"; Open is not
   offered.
6. Remove a finished download from the list (keep the file), paste its link: still found (from the history); Open and
   Show in folder work.
7. Put a file of your own named `test.zip` in the Compressed folder. Add dialog > Check link on another link whose file
   is `test.zip`: the "File name" row says another file of that name is there (contents may differ), and gives the new
   name. Download: saved as `test (1).zip`; your file is untouched.
8. Restart the app: steps 1 and 6 still find the downloads.

## V. Live preview on each OS (once per OS, after installing the release package)
1. Linux: install the `.deb` (or `.rpm`) on a clean system. The package manager also installs the GStreamer
   plugins it recommends (`apt` / `dnf` shows them). The AppImage runs without them. See also AC.
2. Start downloading a large MP4 (and an MP3), press **Watch while downloading** at once: picture or sound within a
   few seconds; it keeps playing as the download goes on, never stops with an error; seeking back works.
3. Throttle the download (a low speed limit), then press Watch: the start still comes first (the "ready from the
   start" figure grows), playback starts and continues.
4. macOS: the same on a downloading MP4 (H.264): it plays while downloading. A WebM / MKV that the system cannot
   decode says "cannot be played here" after a few seconds; the download continues.
5. Windows: unchanged; the same checks pass.

## W. Download Profiles (once per OS)
1. Fresh install or existing user: Settings > Speed > Download profiles shows Default active; "In force now" equals
   your settings; nothing else changed.
2. Start two large downloads with 3 at once. Choose **Low resource**: the running ones keep going (no restart, the
   progress does not jump back); the connection count drops to 4 or fewer within seconds; a third one added waits.
3. Pause one download by hand, choose **Fast**, then a profile of your own with 6 at once: it stays paused.
4. Choose **Balanced**: the dashboard's Smart throttle shows it working (automatic mode), its switch says the
   profile turned it on; your own switch in Settings is unchanged. Browse a heavy page: downloads give way.
5. Change "downloads at once" in the Queue view while Low resource is active: the note says the profile uses 1;
   choose Default: your new value is in force.
6. Duplicate Fast, rename it, set "Speed limit: Limit to 1 MB/s": the speed holds near 1 MB/s; a per-download
   limit of 300 KB/s on one download still applies to it.
7. Delete the active profile of your own: Default is active, downloads continue.
8. Restart the app: the active profile and your profiles are still there.
9. Switch profiles from the status bar: the same as from Settings.

## X. Download again (once per OS)
1. Download a file to the end. Delete it in the system's file manager and switch back to the app: the card and the
   Completed view say "The file is no longer on disk", and the main button is **Download again** (Open is gone).
2. Press it: the same entry downloads from 0 % and finishes; the file is back in the same folder with the same name
   and opens.
3. Without deleting the file, choose "…" > **Download again**: a second file `name (1).ext` appears; the first one
   is untouched.
4. Cancel a running download: its main button is **Download again**; press it: it starts from 0 % and finishes.
5. A failed download: Retry is still the main button; "…" > **Download again** starts it from 0 %.
6. A running or paused download has no "Download again" in its menu.
7. Repeat 1–2 in French and Arabic: the texts are translated, and the layout is right-to-left in Arabic.

## AB. Resume integrity (once per OS: Windows, Linux, macOS)
1. Start a large download (several GB), pause it at about 40 %, then resume: it continues at once (no long
   pause before the speed shows).
2. Pause again. Quit VeloxFetch. With a hex editor, change one byte near the start of the `.part` file. Start
   VeloxFetch and resume: Details → timeline says finished parts "no longer match their check and are downloaded
   again". The percentage drops by about one part only. The finished file matches the publisher's checksum.
3. While a download runs, end VeloxFetch from the task manager / `kill -9`. Start it and resume: the timeline says
   the finished parts were checked again, "all intact". The download continues from where it was (not 0 %).
4. Repeat 1–3 in French and Arabic: the texts are translated.

## AA. Use a new link (once per OS: Windows, Linux, macOS)
1. Start a large download, pause it at about 30 %. Right-click its card: **Use a new link…** is there (it is not
   there for a running or finished download).
2. Paste the same link: "This is the link the download already uses." Nothing changes.
3. Paste another link to the same file (a mirror, or the same file from another address of the same server):
   "Link replaced. Resuming from the saved progress." The download continues from about 30 %, not from 0 %, and
   the finished file opens (compare its checksum with the publisher's when one is given).
4. Pause another download part-way and paste a link to a **different** file: "This link gives a different file.",
   "Your saved progress (…) is kept." The card keeps its percentage. **Download it as a new file** adds a separate
   download.
5. Paste a link that gives a 404: "The new link did not work." with the code; nothing changes.
6. With the browser extension and confirmation on: pause a download from a site, then click the same file's link on
   the page again. The confirmation dialog offers **Continue the stopped download with this link**. Use it: no second
   download appears; the stopped one continues.
7. A failed download whose Doctor says to get a new link: the Details page shows **Use a new link…**.
8. Restart the app right after a switch, and resume: it continues from the new link.
9. Repeat 1–4 in French and Arabic: the texts are translated, and the layout is right-to-left in Arabic.

## Y. Video download details (once per OS)
1. Download a video from a site page, and an HLS stream. While each runs, click its row: the details page opens.
   Percent, speed, the live chart, time left and "Running for" move; the status follows Pause / Resume pressed on
   the page.
2. For the stream: the Segments strip fills. For the site page: it says only the percent applies.
3. When finished: 100 %, size in bytes, Time taken, Average, Finished time, folder and the full path of every saved
   file (copy buttons work); the player plays; Open and Show in folder work.
4. Remove the job from the list while its page is open: the page returns to the list.
5. Repeat in French and Arabic (right to left): every text is translated, and no tool name appears anywhere.

## Z. Drop-down lists (once per OS: Windows, Linux, macOS)
1. In dark and in light theme, open several lists (status bar profile, Settings > Speed, Add download > priority,
   the details page's priority): each opens a list in the same colours as the fields, never a grey or black
   system list. Closed, each field has the same colour as the text fields beside it (never a grey button) and a
   small arrow on its end side (the left in Arabic).
2. Pick with the mouse; then focus a list with Tab, press Enter, move with the arrows, type a letter, Enter: the
   value changes and applies. Escape closes without changing.
3. Near the bottom of the window the list opens upwards. In Arabic it lines up on the right.
4. Scroll the page while a list is open: it closes.

## AC. Linux packages (once per distribution family: Debian/Ubuntu, Fedora, Arch)
1. Install the `.deb` (or `.rpm`) with `apt install ./VeloxFetch_*.deb` (`dnf install ./...rpm`): it installs and
   configures; the GStreamer packages come along as recommended ones when the system can install them.
2. On a system where `gstreamer1.0-libav` cannot be installed (or with `--no-install-recommends`): the app still
   installs and starts; an H.264 preview says "can't play" after a few seconds, the download is unaffected.
3. Run the AppImage on a machine with an NVIDIA or Intel / AMD GPU (VA-API drivers installed): it starts, and the
   preview of an MP4 plays. `unsquashfs -l VeloxFetch*.AppImage | grep gstreamer-1.0/` lists no `va`, `vaapi`,
   `nvcodec`, `qsv`, `v4l2codecs`, `vulkan` or `kms` plugin.

## AD. No flashing when a download starts (once per OS: Windows, Linux, macOS)
1. Have a few finished downloads and two paused ones. On Downloads, resume one paused download, then add a new
   link: the new card fades in once; no other card blinks, fades or jumps.
2. On Active, pause and resume downloads: cards move between Running, Waiting and Paused without fading.
3. Watch a running card's connection grid while its speed changes (a speed limit switched on and off): the lit
   cells keep pulsing smoothly, faster or slower, never all blinking at once.

## AE. Sign-in in the browser confirmation (once per OS: Windows, Linux, macOS)
1. With "Ask me before a download from the browser starts" on, download from the browser a file on a site that asks
   for a user name and password (HTTP authentication): the popup's "Sign in to this site" part is open and says the
   server asks for them.
2. Type only a password, press Start: "A password needs a user name." and the popup stays. Type the user name: the
   message goes. Start: the file downloads.
3. Wrong password: the download fails with "Sign in…" on its card and in its details; signing in there works.
4. On a normal site the part stays closed and nothing changes.

## AF. A slow server (issue #21; once per OS: Windows, Linux, macOS)
1. Download a large file from a slow server (10–100 KB/s), and the same file in a browser at the same time, for 10
   minutes. VeloxFetch's saved size is at least the browser's.
2. In its details, Diagnostics: no "connection lost" lines while the connection count went down.
3. A server that is slow to answer and breaks the connection (a file prepared on request, over a flaky line): the
   download goes on from where it was ("Retrying", then the size keeps growing); it never falls back to 0 B, and the
   finished file is exact (compare with the browser's copy).

## AG. Updates (once per OS and installation kind: Windows setup, AppImage, .deb, .rpm, macOS .dmg, a portable zip)
Needs two signed releases (x.y.z installed, a newer one published with `latest.json` + `latest.json.sig`).
1. Settings > Updates shows the installed version; **Check now** says "Version … is available (size)"; the status bar
   says "Update … available" and opens Settings > Updates when clicked.
2. **Download the update**: the progress moves, then "downloaded and verified"; the file is in `<app data>/updates`.
3. **Install and restart** (Windows setup, AppImage): the administrator prompt (Windows), the app closes, the new
   version starts, Settings > Updates says "up to date", downloads and settings are kept. **Open the installer**
   (`.deb`, `.rpm`, `.dmg`): the system's installer opens with the new version. Portable: **Open the release page**.
4. Replace `latest.json.sig` on a test release by one made with another key: "signature is not valid", nothing offered.
5. Change one byte of the downloaded file in `<app data>/updates` before pressing Install: refused, "download it again".
6. Turn off **Check for updates automatically**, restart, wait two minutes: no request to github.com (a network
   monitor); **Check now** still works.
7. Offline: "Could not reach the update server"; the app keeps working.

## AH. Performance (once per OS, on a fast line)
1. Download a large file (several GB) from a fast server: the speed matches what a browser gets, or more with a server
   that caps each connection; the card shows "Completed" within a moment of reaching 100 % (no long "Verifying").
2. With downloads running, the window scrolls smoothly and buttons answer at once.
3. Optional: `velox-bench run --sizes 1G --conns 1,16,64` (`docs/benchmarks.md`) on that machine; every file "exact".

## AM. Resume on everyday servers (once per OS)
1. Start a large download from a site served by nginx or Apache (many Linux mirrors: a `.iso` of a few GB), pause
   at about 30 %, quit VeloxFetch, open it again and resume: the size goes on from about 30 %, never back to 0 B.
   Details > Diagnostics: "Continuing from …", no "starting over".
2. The same with the network unplugged at resume: it fails ("Retry" offered) and still shows about 30 %; plug the
   network back in, Retry: it continues from there.

## AN. Network usage kept (issue #45; once per OS and installation kind)
1. Download a few files. The dashboard's **Network usage** card shows a total and two rows, "Today" and "This
   month", whose numbers are never cut (also in a small window and in every language); hovering it shows what was
   received since launch.
2. Delete every download (with and without the files) and clear the History: the card does not change.
3. Quit VeloxFetch and open it again, then install an update or reinstall the same version over it: the total is the
   same, and grows again with the next download.
4. The next day, "Today" starts again from 0 while the total and the month keep their bytes (the month from 0 on the
   1st).

## AL. Publishing a release (owner, once per release)
1. `bash scripts/publish-release.sh --key <drive>/update-signing-key.json --merge <Windows release> --merge <macOS
   release>`: it builds, runs the full gate, asks the passphrase once, and ends with "Ready".
2. `release/publish/v<version>/files`: every system's installer, the portable zips, the extension zips,
   `SHA256SUMS.txt`, `latest.json` and `latest.json.sig`. `RELEASE_NOTES.md` lists them by system.
3. Open `repo/README.md` in a Markdown viewer: the pictures show, the links go to `IlyesBs/VeloxFetch-releases`.
4. After publishing on GitHub: an older installed copy (Settings > Updates > Check now) finds the new version, and
   downloading it from the release page gives a file whose SHA-256 is the line in `SHA256SUMS.txt`.

## AK. Arabic layout (once per OS)
1. Settings > Language > العربية, with a few downloads in the list: the window has a vertical scrollbar on the
   downloads (on the left on Linux) and never a sideways scrollbar along the bottom; nothing is cut at either edge.

## AJ. Numbers and names in lists (once per OS)
1. Queue page, a download running: its line says "host · 12.3%" (one decimal), never a long number.
2. Pause the queue, add a link, then Add download > the same link > Check link: "In your list: <file name> · same
   link", with the name the card shows (never an empty name).

## AI. Languages (once per OS)
1. Settings > Appearance > Language: Français, then العربية. Every page, Settings section and dialog reads in that
   language (Arabic from right to left).
2. A failed download (a broken link): its card, its details (error, Doctor, timeline) and the Queue page say it in
   that language; the technical detail (HTTP status, error code) may stay as it is.
3. A refused action (for example a proxy address without a port): the message is in that language.
