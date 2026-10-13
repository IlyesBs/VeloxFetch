## VeloxFetch {{VERSION}}

The download manager that finishes what it starts: up to 64 connections per file, resume after a crash, a power cut
or an expired link, plain-language answers when something fails, and downloads that give way to your games and calls.
Free, no account, no ads, no tracking. For {{SYSTEMS}}.

### Highlights
* **Fast:** up to 64 connections per file, adjusted live; speed limits for everything, per type and per download;
  Fast / Balanced / Low resource profiles.
* **Never lose a download:** progress saved every half second and checked piece by piece; resume after a crash or a
  power cut; **Use a new link** when a link expires; never mixes two versions of a file.
* **Download Doctor:** what failed, where, whether retrying helps, and what to do.
* **Download Intelligence:** size, free space, resume support, temporary links and duplicates checked before you start.
* **Smart throttle:** slows down while you browse, play (League of Legends, Valorant, CS2…) or call (Zoom, Teams…).
* **Smart queues:** night windows, Wi-Fi only, not on mobile data, when idle; shut down or sleep when all is done.
* **Videos:** video pages and HLS / DASH streams in the quality you pick; MP3 / M4A / Opus sound only; watch while
  it downloads.
* **Browser extension** for Chrome, Edge, Brave and Firefox; sign-in protected downloads work.
* **English, French and Arabic**, dark and light themes; signed automatic updates (you choose when to install).

### Downloads

{{DOWNLOADS}}

`latest.json` and `latest.json.sig` are for the app's automatic updates (signed by the developer). `SHA256SUMS.txt`
lists the fingerprint of every file; the `sbom` files list what the programs are made of, for IT departments.

### Install
* **Windows:** run the setup. If Windows says *"Windows protected your PC"*: **More info** > **Run anyway** (the app has
  no paid certificate yet). The portable zip runs without installing.
* **macOS:** open the `.dmg` and drag VeloxFetch to Applications. If macOS refuses to open it: **System Settings > Privacy & Security > Open Anyway**. <!-- mac -->
* **Linux:** `sudo apt install ./VeloxFetch_{{VERSION}}_amd64.deb`, `sudo dnf install ./VeloxFetch-{{VERSION}}-1.x86_64.rpm`,
  or `chmod +x` the AppImage and run it.

### Check your download
Compare your file's SHA-256 with its line in `SHA256SUMS.txt`:
`Get-FileHash <file> -Algorithm SHA256` (Windows PowerShell) or `sha256sum <file>` (Linux).
