# VeloxFetch: texts for download sites, stores and posts

Copy what each site asks for. Every claim here is true of version {{VERSION}} and is shown in the app or measured
(`docs/benchmarks.md`). Pictures: the `screenshots/` folder of the public repository (1280×800, PNG).

## 1. The basics (every form asks for these)

| Field | Value |
|---|---|
| Name | VeloxFetch |
| Version | {{VERSION}} |
| Developer / publisher | Ilyes Ben Salah |
| Price / licence | Free (freeware). No ads, no account, no in-app purchases |
| Category | Internet > Download managers (also: Utilities, Productivity) |
| Systems | Windows 10/11 (64-bit), macOS 12+ (Apple silicon and Intel), Linux (Ubuntu 22.04+, Debian 12+, Fedora 37+, any distribution through the AppImage) | <!-- mac -->
| Systems | Windows 10/11 (64-bit), Linux (Ubuntu 22.04+, Debian 12+, Fedora 37+, any distribution through the AppImage) | <!-- no-mac -->
| Languages | English, French, Arabic |
| Download page | https://github.com/IlyesBs/VeloxFetch-releases/releases/latest |
| Home page | https://github.com/IlyesBs/VeloxFetch-releases |
| Support | https://github.com/IlyesBs/VeloxFetch-releases/issues |
| Privacy | No tracking, no telemetry; connects only to the sites you download from and to the release page for updates |
| Keywords / tags | download manager, download accelerator, resume downloads, multi-connection downloader, video downloader, HLS downloader, download scheduler, bandwidth limiter, browser integration, IDM alternative, free download manager, {{PLATFORMS_LIST}} |

## 2. Taglines

* **≤ 50 characters:** Fast, crash-safe downloads that explain themselves.
* **≤ 80 characters:** Free download manager: 64 connections, crash-safe resume, plain-language error help.
* **≤ 132 characters (Chrome / Edge store short description):** Send downloads from your browser to VeloxFetch: faster
  downloads, crash-safe resume, scheduling, and clear help when one fails.

## 3. Short description (about 250 characters)

VeloxFetch is a free download manager for {{PLATFORMS}}. It downloads each file over up to 64
connections, resumes after a crash, a power cut or an expired link, explains every failure in plain words, and slows
down while you play or call.

## 4. Long description (plain text, no formatting: Softpedia, Uptodown, MajorGeeks, FileHorse, SourceForge…)

VeloxFetch is a free download manager that finishes what it starts. It splits every file into pieces and downloads
them over up to 64 connections at once, adding connections only while they make the download faster. Progress is
saved every half second in an order that survives a crash or a power cut, and each finished piece is fingerprinted,
so a download always continues from the bytes really on your disk and never mixes two versions of a file.

When a link expires in the middle of a download, paste a fresh link to the same file: VeloxFetch proves it is the
same file and continues where it stopped. When something fails, Download Doctor tells you what happened, where the
problem is (the server, your connection, your computer or the link), whether retrying will help, and what to do.
Before a download starts, Download Intelligence checks the size, the free space, the disk's limits, whether the
server can resume, and whether you already have the file.

Smart throttle keeps your games and calls smooth: downloads slow down while you browse, play or call, and take the
whole line back when it is free. Smart queues run big downloads at night, only on Wi-Fi or never on mobile data,
and can shut down or put the computer to sleep when everything is done.

VeloxFetch also downloads videos from video pages and HLS / DASH streams in the quality you choose (MP4, MKV, WebM,
or the sound only as MP3, M4A or Opus), and you can watch a video while it downloads. A browser extension for
Chrome, Edge, Brave and Firefox sends links to the app, including downloads that need you to be signed in.

Interface in English, French and Arabic, dark and light themes, tray icon, notifications, keyboard shortcuts, and
signed automatic updates that never install without your click. No account, no ads, no tracking.

## 5. Feature list (plain bullets)

* Up to 64 connections per file, adjusted live
* Crash-safe resume (progress saved every 500 ms, pieces checked with SHA-256)
* Use a new link: continue a download from a fresh link to the same file
* Download Doctor: clear diagnosis and fix for every failed download
* Download Intelligence: size, free space, resume support and duplicates checked before starting
* Smart duplicate detection: never download the same file twice by mistake
* Smart throttle: gives way to browsing, games and calls
* Smart queues with schedules, Wi-Fi only, not on mobile data, when idle
* Shut down, sleep or hibernate when downloads finish
* Speed limits: global, per file type, per download
* Download profiles: Fast, Balanced, Low resource
* Add many links at once, import a .txt list, or use a pattern like file_[001-100].zip
* Video pages and HLS / DASH streams, sound-only MP3 / M4A / Opus, watch while downloading
* Browser extension for Chrome, Edge, Brave and Firefox
* Sorting into folders by type; never overwrites a file
* History with search and Download again
* English, French, Arabic; dark and light themes
* Signed automatic updates; no ads, no account, no tracking

## 6. What's new in {{VERSION}}

First public release. Everything above, plus: one version everywhere; every message in the user's language
(English, French, Arabic); drop-down lists drawn in the app's colours on every system; signed automatic updates.

## 7. Screenshot captions (in this order)

| File | Caption |
|---|---|
| `dashboard-dark.png` | Three downloads at 78 MB/s, up to 64 connections each |
| `details-dark.png` | One download in detail: pieces, speed, connections |
| `doctor-dark.png` | Download Doctor explains a failed download and how to fix it |
| `smart-throttle-dark.png` | Smart throttle: downloads give way to your games and calls |
| `queue-dark.png` | Smart queue with a night window |
| `add-analysis-dark.png` | Download Intelligence checks a link before you download |
| `batch-dark.png` | 24 lessons added with one pattern |
| `video-dark.png` | Download a streamed video in the quality you choose |
| `arabic-dark.png` | Fully translated: Arabic, right to left |
| `dashboard-light.png` | Light theme |
| `settings-dark.png` | Download profiles: Fast, Balanced, Low resource |

## 8. Where to publish (all free) and what each needs

| Where | What to do | Notes |
|---|---|---|
| **GitHub** (`IlyesBs/VeloxFetch-releases`) | The kit's `repo/` folder (README + screenshots), then the release | Done by `scripts/publish-release.sh` (`docs/publishing.md`) |
| **AlternativeTo** | Add the app, then list it as an alternative to Internet Download Manager, Free Download Manager, JDownloader, Xtreme Download Manager and Motrix | Text §3 and §4, tags §1, pictures §7. Users find apps here by "alternative to IDM" |
| **Softpedia, MajorGeeks, Uptodown, FileHorse** | Their "submit software" form | Text §4, features §5, captions §7; they link to your release page or re-host the files (they keep your SHA256SUMS) |
| **SourceForge** | A project that mirrors the GitHub releases (it can import them) | More download traffic and its own search |
| **winget** (Windows Package Manager) | A pull request to `github.com/microsoft/winget-pkgs` with `wingetcreate new <link to the .exe>` | Users then type `winget install VeloxFetch` |
| **Scoop** | A manifest in the `Extras` bucket (`github.com/ScoopInstaller/Extras`) using the portable zip | Popular with power users |
| **AUR** (Arch Linux) | A `velox-fetch-bin` PKGBUILD that uses the `.deb` or the AppImage | Arch users install with `yay -S velox-fetch-bin` |
| **Microsoft Store** | A developer account (free for individuals at the time of writing: check the current terms); submit the setup | Store installs show no SmartScreen warning |
| **Chrome Web Store** ($5 once), **Edge Add-ons** (free), **Firefox Add-ons** (free) | The extension zips (`docs/store-publishing.md`) | Describe it as "sends links to the VeloxFetch app" (tagline §2). Chrome's store does not accept extensions that download from YouTube: if review objects to the video bar, publish it unlisted, or keep the manual install |
| **Product Hunt** | A launch with the tagline, §3, and the pictures | Answer every comment on launch day |
| **Reddit** | r/software, r/windows, r/linux, r/DataHoarder; r/techsupport only to answer someone who asks | Post §9; read each subreddit's self-promotion rules first |

## 9. Posts

**English (Reddit, X, Facebook, forums)**

> I made VeloxFetch, a free download manager for {{PLATFORMS}}. It downloads over up to 64 connections,
> resumes after crashes, power cuts and expired links, and when a download fails it tells you why in plain words and
> how to fix it. It also slows itself down while you game or call, so your ping stays low. No ads, no account, no
> tracking. English, French and Arabic. Feedback welcome!
> https://github.com/IlyesBs/VeloxFetch-releases

**Français**

> J'ai créé VeloxFetch, un gestionnaire de téléchargements gratuit pour {{PLATFORMS_FR}}. Jusqu'à 64
> connexions par fichier, reprise après un plantage, une coupure de courant ou un lien expiré, et quand un
> téléchargement échoue, il explique pourquoi en mots simples et comment le réparer. Il ralentit tout seul pendant
> vos jeux et vos appels. Sans publicité, sans compte, sans pistage. Vos retours sont les bienvenus !
> https://github.com/IlyesBs/VeloxFetch-releases

**العربية**

> طوّرت VeloxFetch، مدير تنزيلات مجاني لـ{{PLATFORMS_AR}}. يحمّل الملف عبر ما يصل إلى 64 اتصالًا، ويستأنف بعد تعطل
> الجهاز أو انقطاع الكهرباء أو انتهاء صلاحية الرابط، وعندما يفشل تنزيل يشرح لك السبب بكلمات بسيطة وكيف تصلحه. يخفّف
> السرعة تلقائيًا أثناء اللعب والمكالمات. بدون إعلانات، بدون حساب، بدون تتبع. الواجهة بالعربية والفرنسية والإنجليزية.
> https://github.com/IlyesBs/VeloxFetch-releases

## 10. Descriptions in French and Arabic (sites in those languages)

**Français — courte :** Gestionnaire de téléchargements gratuit : jusqu'à 64 connexions, reprise après plantage ou
lien expiré, diagnostic clair des échecs, et il s'efface pendant vos jeux et vos appels.

**Français — longue :** VeloxFetch découpe chaque fichier et le télécharge par jusqu'à 64 connexions à la fois. La
progression est enregistrée toutes les demi-secondes : après un plantage ou une coupure de courant, le téléchargement
reprend là où il s'était arrêté, sans jamais mélanger deux versions d'un fichier. Un lien expiré ? Collez un nouveau
lien vers le même fichier et le téléchargement continue. En cas d'échec, le Docteur des téléchargements explique ce
qui s'est passé et quoi faire. Files d'attente de nuit, Wi-Fi uniquement, arrêt de l'ordinateur à la fin, vidéos
dans la qualité choisie, extension pour Chrome, Edge, Brave et Firefox. Sans publicité, sans compte, sans pistage.

**العربية — قصير:** مدير تنزيلات مجاني: حتى 64 اتصالًا، استئناف بعد التعطل أو انتهاء صلاحية الرابط، شرح واضح لأسباب
الفشل، ويخفّف السرعة أثناء اللعب والمكالمات.

**العربية — طويل:** يقسم VeloxFetch كل ملف ويحمّله عبر ما يصل إلى 64 اتصالًا في آن واحد. يُحفظ التقدّم كل نصف ثانية:
بعد تعطل الجهاز أو انقطاع الكهرباء يستأنف التنزيل من حيث توقف دون أن يخلط نسختين من الملف. انتهت صلاحية الرابط؟ الصق
رابطًا جديدًا للملف نفسه ويكمل التنزيل. وعند الفشل يشرح "طبيب التنزيلات" ما حدث وما يجب فعله. قوائم انتظار ليلية،
Wi-Fi فقط، إيقاف تشغيل الجهاز عند الانتهاء، تنزيل الفيديو بالجودة التي تختارها، وإضافة للمتصفحات Chrome وEdge وBrave
وFirefox. بدون إعلانات، بدون حساب، بدون تتبع.
