//! The app's side of the browser integration: serves the local link the native host talks to, turns
//! validated handoffs into downloads, and lets the user register / remove the host with browsers.
//!
//! Nothing here decides what is safe: `browser-bridge` validated the message twice already, and the
//! download manager validates the URL, file name and headers again before anything is stored.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::identity::FIREFOX_STORE_URL;
use browser_bridge::launch as app_start;
use browser_bridge::server::{BoxFuture, DownloadAccepted, Handler, HandlerError, ServerConfig};
use browser_bridge::{
    chromium_ids, serve, Allowlist, Caller, DownloadRequest, Endpoint, Listener, MediaHint, Source, Token, CHROME_EXTENSION_ID,
    CHROME_STORE_ID, FIREFOX_EXTENSION_ID, HOST_NAME,
};
use download_manager::{ApiError, BrowserContext, DownloadManager, ManagerError, NewDownload};

/// Called when a download is waiting for the user (brings the window to the front).
pub type Notify = Arc<dyn Fn() + Send + Sync>;
use serde::{Deserialize, Serialize};
use system_integration::browser_launch as launch;
use system_integration::native_messaging::{self as nm, Browser, HostSpec, Status, Target};
use system_integration::sandboxed_browsers::{self as sandboxed, SandboxedBrowser};
use system_integration::store_extension as store;
use system_integration::system::{NativeSystem, ProtocolHandler, SystemIntegration};
use system_integration::Platform;

/// Turns browser handoffs into downloads.
struct AppHandler {
    manager: DownloadManager,
    notify: Notify,
    contacts: Arc<ContactLog>,
}

/// When a VeloxFetch extension last reached the app (from any browser): proof that it is installed and working.
/// Only a time and where the extension came from; nothing about pages or links.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionContact {
    /// Milliseconds since 1970.
    pub at: u64,
    /// The extension from the Chrome Web Store (not the developer copy, nor Firefox's add-on).
    pub from_store: bool,
}

/// Told to the window (the `velox://extension` event).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ExtensionEvent {
    /// A contact was saved. `announce`: the first contact ever, or the one the app waited for since the first start;
    /// the window says "installed and connected".
    Contact { contact: ExtensionContact, announce: bool },
    /// The first start waits for the browser to add the extension by itself, since `since` (ms since 1970).
    Waiting { since: u64 },
}

pub type OnExtensionEvent = Arc<dyn Fn(ExtensionEvent) + Send + Sync>;

/// The extension's contacts, kept in the data folder (`browser-extension/contact.json`) so Settings knows after a
/// restart too, and the first start's wait (`browser-extension/.awaiting`: since when the app waits for the browser
/// to add the extension by itself).
struct ContactLog {
    dir: PathBuf,
    /// The last contact, and when it was last saved (saved at most every ten minutes: the extension pings often).
    last: Mutex<(Option<ExtensionContact>, Option<std::time::Instant>)>,
    on_event: OnExtensionEvent,
}

const SAVE_CONTACT_EVERY: std::time::Duration = std::time::Duration::from_secs(600);

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

impl ContactLog {
    fn open(data_dir: &Path, on_event: OnExtensionEvent) -> Self {
        let dir = data_dir.join("browser-extension");
        let saved = std::fs::read(dir.join("contact.json")).ok().and_then(|b| serde_json::from_slice::<ExtensionContact>(&b).ok());
        Self { dir, last: Mutex::new((saved, None)), on_event }
    }

    fn last(&self) -> Option<ExtensionContact> {
        self.last.lock().unwrap_or_else(|p| p.into_inner()).0
    }

    /// Since when the app waits for the browser to add the extension (`None`: not waiting).
    fn awaited_since(&self) -> Option<u64> {
        std::fs::read_to_string(self.dir.join(".awaiting")).ok().and_then(|s| s.trim().parse().ok())
    }

    fn start_waiting(&self) -> std::io::Result<()> {
        let since = now_ms();
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(self.dir.join(".awaiting"), since.to_string())?;
        (self.on_event)(ExtensionEvent::Waiting { since });
        Ok(())
    }

    fn record(&self, caller: &Caller) {
        let from_store = matches!(caller, Caller::Chromium { extension_id } if Some(extension_id.as_str()) == CHROME_STORE_ID);
        let contact = ExtensionContact { at: now_ms(), from_store };
        let mut last = self.last.lock().unwrap_or_else(|p| p.into_inner());
        let first = last.0.is_none();
        let save = first || last.0.map(|c| c.from_store) != Some(from_store) || last.1.is_none_or(|t| t.elapsed() >= SAVE_CONTACT_EVERY);
        last.0 = Some(contact);
        if !save {
            return;
        }
        last.1 = Some(std::time::Instant::now());
        drop(last);
        let awaited = self.awaited_since().is_some();
        let written = std::fs::create_dir_all(&self.dir).and_then(|_| {
            let tmp = self.dir.join("contact.json.tmp");
            std::fs::write(&tmp, serde_json::to_vec(&contact).unwrap_or_default())?;
            std::fs::rename(&tmp, self.dir.join("contact.json"))
        });
        if let Err(e) = written {
            tracing::warn!(error = %e, "could not save the extension's contact");
        }
        if awaited {
            let _ = std::fs::remove_file(self.dir.join(".awaiting"));
            tracing::info!(from_store, "the browser extension arrived and reached the app");
        }
        (self.on_event)(ExtensionEvent::Contact { contact, announce: first || awaited });
    }
}

impl Handler for AppHandler {
    fn app_version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    fn video_bar(&self) -> bool {
        self.manager.video_bar()
    }

    fn contacted(&self, caller: &Caller) {
        self.contacts.record(caller);
    }

    fn download(&self, req: DownloadRequest, caller: Caller) -> BoxFuture<'_, Result<DownloadAccepted, HandlerError>> {
        Box::pin(async move {
            tracing::info!(origin = %caller.origin(), source = ?req.source, media = ?req.media, "download handed over by a browser");
            let stream = url::Url::parse(&req.url).is_ok_and(|u| download_manager::is_stream_link(&u));
            let media = req.media.or(stream.then_some(MediaHint::Stream));
            let new = NewDownload { url: req.url, filename: req.filename, ..Default::default() };
            let ctx = BrowserContext { referrer: req.referrer, cookies: req.cookies, user_agent: req.user_agent, incognito: req.incognito };
            // A download taken from the browser that only the signed-in browser can get (SharePoint, Office Online,
            // members-only sites) is declined: the extension then lets the browser download it with its own session.
            if media.is_none() && req.source == Source::Intercept {
                if let Err(ManagerError::InvalidInput(m)) = self.manager.browser_can_take_over(&new, &ctx).await {
                    return Err(HandlerError { code: "invalid_input", message: m });
                }
            }
            let result = if let Some(hint) = media {
                // A video always waits for the user: the quality is chosen in the video dialog.
                self.manager.add_pending_media(&new.url, ctx, hint == MediaHint::Page).await.map(|p| {
                    (self.notify)();
                    DownloadAccepted { id: 0, filename: p.filename }
                })
            } else if self.manager.browser_confirm() {
                // Parked until the user confirms in the app's dialog. `id: 0` tells the extension
                // "received, waiting for the user": the browser's own download can be dropped.
                self.manager.add_pending(new, ctx).await.map(|p| {
                    (self.notify)();
                    DownloadAccepted { id: 0, filename: p.filename }
                })
            } else {
                self.manager.create_download_from_browser(new, ctx).await.map(|d| DownloadAccepted { id: d.id, filename: d.filename })
            };
            match result {
                Ok(a) => Ok(a),
                Err(ManagerError::InvalidInput(m)) => Err(HandlerError { code: "invalid_input", message: m }),
                Err(_) => Err(HandlerError { code: "internal", message: "could not add the download".into() }),
            }
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetBrowserIntegration {
    pub browser: Browser,
    pub enabled: bool,
}

/// `browser: None` means "the user's default browser".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallExtension {
    pub browser: Option<Browser>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionInstallDto {
    pub browser: Browser,
    /// "store": the browser offers the published extension itself (its "New extension added" prompt, or the
    /// store page that was opened); "developer": the user loads the folder below in Developer mode.
    pub method: &'static str,
    /// Folder holding the extension, to be picked in the browser (Load unpacked / temporary add-on). Empty for the
    /// store.
    pub folder: String,
    /// The extension's page in the Chrome Web Store (store only; empty otherwise).
    pub store_url: String,
    /// The browser was found and opened (on the store page, or on its extensions page).
    pub opened_browser: bool,
    pub opened_folder: bool,
    pub integration: BrowserIntegrationDto,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserEntry {
    pub browser: Browser,
    pub registered: bool,
    /// The OS can register this browser (false, for instance, on an unsupported platform).
    pub supported: bool,
    /// It is the user's default browser.
    pub is_default: bool,
    /// The browser is installed on this computer (the Brave line also covers other Chromium browsers,
    /// so "not found" is a hint, not an error).
    pub found: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserIntegrationDto {
    /// The local link is up: a running extension can reach the app.
    pub link_running: bool,
    /// The native host program exists next to the application.
    pub host_present: bool,
    pub browsers: Vec<BrowserEntry>,
    pub chrome_extension_id: String,
    /// The extension's id in the Chrome Web Store: Chrome, Edge and Brave get it from there.
    pub chrome_store_id: Option<String>,
    pub firefox_extension_id: String,
    /// The Firefox Add-ons (AMO) listing Firefox gets the add-on from (`None` = not published yet).
    pub firefox_store_url: Option<String>,
    /// Ask the user (a dialog with the file's details) before a browser download starts.
    pub confirm_downloads: bool,
    /// Show the "Download video" bar over videos in the browser.
    pub video_bar: bool,
    /// Cookies and credentials handed over by a browser survive a restart, sealed with a key from
    /// the operating system's credential store. When false they are kept in memory only.
    pub secrets_protected: bool,
    /// The last time a VeloxFetch extension reached the app: it is installed and working. `None`: never yet.
    pub extension_contact: Option<ExtensionContact>,
    /// Since when (ms since 1970) the app waits for the browser to add the extension by itself (first start, through
    /// the installer's pointer). `None`: not waiting.
    pub extension_awaited_since: Option<u64>,
    /// After this long without the extension, the window offers its store page instead.
    pub extension_wait_ms: u64,
}

pub struct Bridge {
    manager: DownloadManager,
    resource_dir: Option<PathBuf>,
    link_running: bool,
    manifest_dir: PathBuf,
    spec: HostSpec,
    targets: Vec<(Browser, Option<Target>)>,
    home: Option<PathBuf>,
    /// The home the other browsers of each line are looked for in (`nm::sibling_targets`): the real one in release
    /// builds, a test's scratch home in debug builds (none otherwise).
    sibling_home: Option<PathBuf>,
    /// Debug builds only (tests): where the store pointer goes instead of the real place (a scratch home, or a
    /// scratch registry key on Windows).
    store_scratch: Option<StoreScratch>,
    /// Linux: the root the packages' pointers are looked for under (`/`; a scratch folder in tests).
    system_root: PathBuf,
    contacts: Arc<ContactLog>,
    /// Where to look for browsers installed as snap / Flatpak packages (Linux): `(home, system root)`.
    sandbox_roots: Option<(PathBuf, PathBuf)>,
    /// The token and handler of the link this instance serves, to serve it in more folders.
    link: Option<(Token, Arc<AppHandler>)>,
    /// Sandbox folders the link is already served in.
    served: Mutex<HashSet<PathBuf>>,
    /// Linux release builds: the `veloxfetch://` link that lets a sandboxed browser's helper have the desktop start
    /// the app when it is closed (registered while a sandboxed browser is connected).
    start_link: Option<ProtocolHandler>,
}

/// Debug builds only (end-to-end tests): register into a scratch registry key instead of the real
/// browsers' keys, and use a private pipe. Release builds ignore both variables.
fn dev_var(name: &str) -> Option<String> {
    cfg!(debug_assertions).then(|| std::env::var(name).ok()).flatten().filter(|v| !v.is_empty())
}

/// Who holds the local link the browsers' helper connects to. It is also what keeps VeloxFetch to one
/// running copy per user: a second copy would open the same database and race the first one's queue.
pub enum LinkClaim {
    /// This instance owns the link.
    Ours(Listener),
    /// Another VeloxFetch of this user is running.
    TakenByAnotherInstance,
    /// The link could not be opened for another reason: run without browser integration.
    Unavailable,
}

/// Claim the link. Call before anything else is opened.
pub fn claim_link(data_dir: &Path) -> LinkClaim {
    let endpoint = dev_var("VELOX_BRIDGE_ENDPOINT").map(Endpoint).unwrap_or_else(|| Endpoint::default_for(data_dir));
    match tauri::async_runtime::block_on(async { Listener::bind(&endpoint) }) {
        Ok(listener) => LinkClaim::Ours(listener),
        Err(e) if browser_bridge::owned_by_another_instance(&e) => LinkClaim::TakenByAnotherInstance,
        Err(e) => {
            tracing::warn!(error = %e, "browser bridge: cannot open the local link; browser integration is off");
            LinkClaim::Unavailable
        }
    }
}

impl Bridge {
    /// Start serving the link (when this instance owns it) and work out where each browser expects its
    /// registration. The token file is only written by the owner of the link: writing it from anywhere
    /// else would lock the browsers out of the instance that really serves them.
    pub fn start(
        data_dir: &Path,
        listener: Option<Listener>,
        manager: DownloadManager,
        resource_dir: Option<PathBuf>,
        notify: Notify,
        on_extension: OnExtensionEvent,
    ) -> Self {
        let contacts = Arc::new(ContactLog::open(data_dir, on_extension));
        let link = match listener {
            None => None,
            Some(listener) => match Token::create_file(data_dir) {
                Err(e) => {
                    tracing::warn!(error = %e, "browser bridge: cannot write the token file; browser integration is off");
                    None
                }
                Ok(token) => {
                    let handler = Arc::new(AppHandler { manager: manager.clone(), notify, contacts: contacts.clone() });
                    tauri::async_runtime::spawn(serve(listener, token.clone(), handler.clone(), server_config()));
                    Some((token, handler))
                }
            },
        };
        let link_running = link.is_some();

        // How the browsers' helper starts this app when a download comes while it is closed. An AppImage runs from
        // a folder that vanishes when it closes: the AppImage file itself is what to start.
        let appimage = std::env::var_os("APPIMAGE")
            .map(PathBuf::from)
            .filter(|p| Platform::current() == Platform::Linux && p.is_absolute() && p.is_file());
        let app = appimage.clone().or_else(|| std::env::current_exe().ok());
        if let (true, Some(app)) = (link_running, &app) {
            if let Err(e) = app_start::record_app(data_dir, app) {
                tracing::warn!(error = %e, "browser bridge: cannot record where the app is; browsers cannot start it");
            }
        }

        // Beside the application (Windows installer, portable, dev builds) or in the platform's
        // resource folder (Linux packages, macOS bundle).
        let host_name = if cfg!(windows) { "velox-native-host.exe" } else { "velox-native-host" };
        let host_exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .into_iter()
            .chain(resource_dir.clone())
            .map(|d| d.join(host_name))
            .find(|p| p.is_file())
            .unwrap_or_default();
        // Registered from inside an AppImage, the helper would be gone as soon as the app closes: use a copy.
        let host_exe = match appimage {
            Some(_) if host_exe.is_file() => sandboxed::copy_helper(&host_exe, data_dir).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "browser bridge: cannot keep a copy of the helper");
                host_exe
            }),
            _ => host_exe,
        };
        let spec = HostSpec {
            name: HOST_NAME.to_string(),
            description: "VeloxFetch browser integration".to_string(),
            host_exe,
            chromium_origins: chromium_ids().iter().map(|id| format!("chrome-extension://{id}/")).collect(),
            firefox_ids: vec![FIREFOX_EXTENSION_ID.to_string()],
        };
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let scratch = dev_var("VELOX_NMH_SCRATCH_KEY");
        // Tests on Linux / macOS: the browsers' folders under a scratch home instead of the real one (debug builds).
        let scratch_home = dev_var("VELOX_NMH_SCRATCH_HOME").map(PathBuf::from);
        let targets = Browser::ALL
            .into_iter()
            .map(|b| {
                let t = match (&scratch_home, &scratch) {
                    (Some(h), _) => nm::default_target(b, Platform::current(), HOST_NAME, Some(h)),
                    (None, Some(key)) => Some(Target::Registry { subkey: format!(r"{key}\{}\{HOST_NAME}", b.as_str()) }),
                    (None, None) => nm::default_target(b, Platform::current(), HOST_NAME, home.as_deref()),
                };
                (b, t)
            })
            .collect();
        // Snap / Flatpak browsers: Linux only. Debug builds look only in a scratch home a test gives (never in the
        // real browsers' folders).
        let sandbox_roots = match (Platform::current(), cfg!(debug_assertions)) {
            (Platform::Linux, false) => home.clone().map(|h| (h, PathBuf::from("/"))),
            (Platform::Linux, true) => dev_var("VELOX_SANDBOX_HOME").map(|h| (PathBuf::from(&h), Path::new(&h).join("system-root"))),
            _ => None,
        };
        let store_scratch = match (&scratch_home, &scratch) {
            (Some(h), _) => Some(StoreScratch::Home(h.clone())),
            (None, Some(key)) => Some(StoreScratch::Key(key.clone())),
            (None, None) => None,
        };
        // Release builds, or a test that gave its own scratch places: never a debug build on the real registrations.
        let may_refresh = !cfg!(debug_assertions) || store_scratch.is_some();
        let bridge = Self {
            manager,
            resource_dir,
            link_running,
            manifest_dir: data_dir.join("native-messaging"),
            spec,
            targets,
            sibling_home: match (&scratch_home, cfg!(debug_assertions)) {
                (Some(h), _) => Some(h.clone()),
                (None, false) => home.clone(),
                (None, true) => None,
            },
            home,
            store_scratch,
            // Tests give a scratch root; a debug build never relies on the real system folders.
            system_root: match dev_var("VELOX_STORE_SYSTEM_ROOT") {
                Some(root) => PathBuf::from(root),
                None if cfg!(debug_assertions) => data_dir.join("no-system-root"),
                None => PathBuf::from("/"),
            },
            contacts,
            sandbox_roots,
            link,
            served: Mutex::new(HashSet::new()),
            start_link: app
                .filter(|_| !cfg!(debug_assertions) && Platform::current() == Platform::Linux)
                .map(|exe| ProtocolHandler { scheme: app_start::SCHEME.into(), exe }),
        };
        if may_refresh {
            bridge.refresh_registrations();
        }
        bridge.refresh_sandboxed();
        bridge
    }

    /// Keep connected browsers working after an update without the user doing anything: a registration of ours that
    /// is out of date (the store extension's id was added, the helper moved) is written again, so the extension
    /// from the store can reach the app at once. A browser the user never connected, or another copy of VeloxFetch's
    /// registration, is left alone.
    fn refresh_registrations(&self) {
        for (b, t) in &self.targets {
            let Some(t) = t else { continue };
            if nm::needs_refresh(*b, &self.spec, t) {
                match nm::install(*b, &self.spec, &self.manifest_dir, t) {
                    Ok(()) => tracing::info!(browser = ?b, "browser registration brought up to date"),
                    Err(e) => tracing::warn!(error = %e, browser = ?b, "could not bring a browser registration up to date"),
                }
            }
            // A connected line: its other browsers (installed since, or connected by an older version) too.
            if nm::status(*b, &self.spec, &self.manifest_dir, t).registered {
                self.install_siblings(*b);
            }
        }
    }

    /// The other browsers of `browser`'s line this user has (Chromium, Vivaldi, Opera... on the Brave line; Firefox's
    /// forks), each in its own folder. Only rewritten when it differs.
    fn siblings(&self, browser: Browser) -> Vec<Target> {
        nm::sibling_targets(browser, Platform::current(), HOST_NAME, self.sibling_home.as_deref())
    }

    fn install_siblings(&self, browser: Browser) {
        for t in self.siblings(browser) {
            if !nm::status(browser, &self.spec, &self.manifest_dir, &t).registered {
                if let Err(e) = nm::install(browser, &self.spec, &self.manifest_dir, &t) {
                    tracing::warn!(error = %e, browser = ?browser, "could not connect another browser of this line");
                }
            }
        }
    }

    /// Where this browser's pointer to the store extension goes for this user.
    fn store_way(&self, browser: Browser, id: &str) -> store::Support {
        match &self.store_scratch {
            None => store::support(browser, Platform::current(), self.home.as_deref(), id),
            Some(StoreScratch::Home(h)) => store::support(browser, Platform::current(), Some(h), id),
            Some(StoreScratch::Key(key)) => match store::support(browser, Platform::current(), None, id) {
                store::Support::Registry { .. } => store::Support::Registry { key: format!(r"{key}\store\{}\{id}", browser.as_str()) },
                other => other,
            },
        }
    }

    /// The snap / Flatpak browsers installed now (looked up every time: one can be installed or run for the first
    /// time while the app is open).
    fn sandboxed(&self) -> Vec<SandboxedBrowser> {
        self.sandbox_roots.as_ref().map(|(home, root)| sandboxed::find(home, root, HOST_NAME)).unwrap_or_default()
    }

    /// Keep sandboxed browsers working without the user doing anything: one connected before gets the current
    /// helper (an update replaced it); one installed since, of a browser the user connected, is connected too.
    /// Both get the link in their folder. A browser the user never connected is left alone.
    fn refresh_sandboxed(&self) {
        let connected = |b: Browser| {
            self.targets
                .iter()
                .any(|(x, t)| *x == b && t.as_ref().is_some_and(|t| nm::status(b, &self.spec, &self.manifest_dir, t).registered))
        };
        for b in self.sandboxed().iter().filter(|b| b.manifest.is_file() || connected(b.browser)) {
            if !sandboxed::registered(b, &self.spec) {
                if let Err(e) = sandboxed::install(b, &self.spec) {
                    tracing::warn!(error = %e, package = b.package, "could not refresh a sandboxed browser's helper");
                    continue;
                }
            }
            self.serve_in(b);
        }
        self.sync_start_link();
    }

    /// Have the `veloxfetch://` link start this app exactly while a sandboxed browser is connected.
    fn sync_start_link(&self) {
        let Some(h) = &self.start_link else { return };
        let system = NativeSystem::new();
        let wanted = self.sandboxed().iter().any(|b| b.manifest.is_file());
        let res = match (wanted, system.protocol_registered(h)) {
            (true, false) => system.register_protocol(h),
            (false, true) => system.unregister_protocol(&h.scheme),
            _ => Ok(()),
        };
        if let Err(e) = res {
            tracing::warn!(error = %e, "browser bridge: cannot update the link that starts the app");
        }
    }

    /// Serve the link in `b`'s own folder, where its helper looks (once per folder).
    fn serve_in(&self, b: &SandboxedBrowser) {
        let Some((token, handler)) = &self.link else { return };
        if !self.served.lock().unwrap_or_else(|p| p.into_inner()).insert(b.data_dir.clone()) {
            return;
        }
        let (dir, token, handler, package) = (b.data_dir.clone(), token.clone(), handler.clone(), b.package);
        tauri::async_runtime::spawn(async move {
            let endpoint = Endpoint::default_for(&dir);
            let listener = std::fs::create_dir_all(&dir).and_then(|_| Listener::bind(&endpoint));
            match listener.and_then(|l| token.write_file(&dir).map(|_| l)) {
                Ok(listener) => {
                    tracing::info!(package, "browser bridge: serving a sandboxed browser");
                    serve(listener, token, handler, server_config()).await;
                }
                Err(e) => tracing::warn!(error = %e, package, "browser bridge: cannot serve a sandboxed browser"),
            }
        });
    }

    pub fn integration(&self) -> BrowserIntegrationDto {
        self.refresh_sandboxed();
        let all_sandboxed = self.sandboxed();
        let platform = Platform::current();
        let default = launch::default_browser(platform);
        let browsers = self
            .targets
            .iter()
            .map(|(b, t)| {
                let status = t.as_ref().map(|t| nm::status(*b, &self.spec, &self.manifest_dir, t));
                let mut sandboxed = all_sandboxed.iter().filter(|s| s.browser == *b).peekable();
                let has_sandboxed = sandboxed.peek().is_some();
                // Connected means every copy of the browser on this computer can reach the app.
                let sandboxed_ok = sandboxed.all(|s| sandboxed::registered(s, &self.spec));
                BrowserEntry {
                    browser: *b,
                    registered: status.as_ref().is_some_and(|s: &Status| s.registered) && sandboxed_ok,
                    supported: t.is_some(),
                    is_default: default == Some(*b),
                    found: has_sandboxed || launch::find_browser(*b, platform).is_some(),
                }
            })
            .collect();
        BrowserIntegrationDto {
            link_running: self.link_running,
            host_present: self.spec.host_exe.is_file(),
            browsers,
            chrome_extension_id: CHROME_EXTENSION_ID.to_string(),
            chrome_store_id: CHROME_STORE_ID.map(str::to_string),
            firefox_extension_id: FIREFOX_EXTENSION_ID.to_string(),
            firefox_store_url: FIREFOX_STORE_URL.map(str::to_string),
            confirm_downloads: self.manager.browser_confirm(),
            video_bar: self.manager.video_bar(),
            secrets_protected: self.manager.secrets_protected(),
            extension_contact: self.contacts.last(),
            extension_awaited_since: self.contacts.awaited_since(),
            extension_wait_ms: dev_var("VELOX_EXTENSION_WAIT_MS").and_then(|v| v.parse().ok()).unwrap_or(180_000),
        }
    }

    /// Whether this browser adds the store extension by itself, with nothing for the user to open: the installer's
    /// pointer is in place (Linux deb / rpm), or the app just wrote the per-user one (Windows, macOS: the browser
    /// shows its own "New extension added"). Not for an AppImage: Linux has no per-user pointer.
    fn browser_adds_it_itself(&self, browser: Browser, id: &str) -> bool {
        match self.store_way(browser, id) {
            store::Support::None => store::system_pointer(browser, &self.system_root, id).is_some_and(|p| p.is_file()),
            way => store::offer(&way)
                .inspect_err(|e| tracing::warn!(error = %e, "pointing the browser at the store extension failed"))
                .unwrap_or(false),
        }
    }

    /// The one-button flow. See [`InstallExtension`]. Never touches anything but our own folders and
    /// the (per-user) browser registration, and starts programs only through fixed arguments.
    pub fn install_extension(&self, req: InstallExtension, resources: Option<&Path>) -> Result<ExtensionInstallDto, ApiError> {
        let platform = Platform::current();
        let err = |message: &str| ApiError { code: "engine", message: message.into() };
        let supported = |b: Browser| self.targets.iter().any(|(x, t)| *x == b && t.is_some());

        let browser = req
            .browser
            .or_else(|| launch::default_browser(platform).filter(|b| supported(*b)))
            .or_else(|| Browser::ALL.into_iter().find(|b| supported(*b) && launch::find_browser(*b, platform).is_some()))
            .unwrap_or(Browser::Brave); // no known browser: the Chromium registration also serves Vivaldi, Opera, ...
        if !supported(browser) {
            return Err(ApiError { code: "invalid_input", message: "this browser is not supported on this system".into() });
        }

        // 1. connect the browser to the app (per user)
        self.set(SetBrowserIntegration { browser, enabled: true })?;

        // 2. Chrome, Edge, Brave: the extension from the Chrome Web Store, never a folder. The browser is pointed at
        // it for this user where the system allows it (its own "New extension added" prompt; the installers already
        // did it for every user), and its store page opens with the browser's own Add button. Tests set
        // VELOX_NO_LAUNCH so they never start real programs (debug builds only).
        let launching = dev_var("VELOX_NO_LAUNCH").is_none();
        if let Some(id) = CHROME_STORE_ID.filter(|_| browser != Browser::Firefox) {
            if let Err(e) = store::offer(&self.store_way(browser, id)) {
                tracing::warn!(error = %e, "pointing the browser at the store extension failed; its store page still opens");
            }
            let opened_browser =
                launching && launch::find_browser(browser, platform).is_some_and(|l| launch::open_store_page(&l, id).is_ok());
            return Ok(ExtensionInstallDto {
                browser,
                method: "store",
                folder: String::new(),
                store_url: launch::store_page_url(id),
                opened_browser,
                opened_folder: false,
                integration: self.integration(),
            });
        }

        // 2b. Firefox: the published add-on on Firefox Add-ons (AMO). Its listing opens with Firefox's own Add
        // button. Firefox has no per-user external-extension pointer (nothing to register), so we only open it.
        if browser == Browser::Firefox {
            if let Some(url) = FIREFOX_STORE_URL {
                let opened_browser =
                    launching && launch::find_browser(browser, platform).is_some_and(|l| launch::open_url(&l, url).is_ok());
                return Ok(ExtensionInstallDto {
                    browser,
                    method: "store",
                    folder: String::new(),
                    store_url: url.to_string(),
                    opened_browser,
                    opened_folder: false,
                    integration: self.integration(),
                });
            }
        }

        // 3. Firefox before its add-on was published, or no store id: the extension files, in a fixed folder inside the app's data folder
        let family = if browser == Browser::Firefox { "firefox" } else { "chromium" };
        let source = dev_var("VELOX_EXTENSION_SRC")
            .map(PathBuf::from)
            .or_else(|| resources.map(|r| r.join("extension")))
            .or_else(|| self.resource_dir.as_ref().map(|r| r.join("extension")))
            .map(|r| r.join(family))
            .filter(|s| s.join("manifest.json").is_file())
            .ok_or_else(|| err("the extension files are missing; reinstall VeloxFetch"))?;
        let data_dir = self.manifest_dir.parent().ok_or_else(|| err("no data folder"))?;
        let folder = data_dir.join("browser-extension").join(family);
        copy_extension(&source, &folder).map_err(|e| {
            tracing::warn!(error = %e, "copying the browser extension failed");
            err("could not prepare the extension folder")
        })?;

        // 4. open the browser on its extensions page, and the folder to pick.
        let opened_browser =
            launching && launch::find_browser(browser, platform).is_some_and(|l| launch::open_extensions_page(browser, &l).is_ok());
        let opened_folder = launching && launch::open_folder(platform, &folder).is_ok();

        Ok(ExtensionInstallDto {
            browser,
            method: "developer",
            folder: folder.to_string_lossy().into_owned(),
            store_url: String::new(),
            opened_browser,
            opened_folder,
            integration: self.integration(),
        })
    }

    /// First start only: set the extension up for the default browser without waiting for the user to find the
    /// button (connect the browser, then the extension's store page; Firefox: its extensions page). Done once per
    /// installation (a marker in the data folder), never in debug builds unless a test asks, and it never fails the
    /// app: whatever goes wrong is logged, and Settings > Browser still has the button.
    pub fn auto_install_once(&self, resources: Option<&Path>) {
        let wanted = !cfg!(debug_assertions) || dev_var("VELOX_AUTO_EXTENSION").is_some();
        let Some(data_dir) = self.manifest_dir.parent() else { return };
        let marker = data_dir.join("browser-extension").join(".offered");
        if !wanted || marker.exists() || !self.spec.host_exe.is_file() {
            return;
        }
        let platform = Platform::current();
        let supported = |b: Browser| self.targets.iter().any(|(x, t)| *x == b && t.is_some());
        let Some(browser) = launch::default_browser(platform).filter(|b| supported(*b) && launch::find_browser(*b, platform).is_some())
        else {
            tracing::info!("no supported default browser found: the extension is set up from Settings");
            return;
        };
        if self.integration().browsers.iter().any(|b| b.browser == browser && b.registered) {
            let _ = mark(&marker); // set up before (an older version, or by hand)
            return;
        }
        // The browser adds the store extension by itself: connect it and wait for the extension to reach the app (the
        // window offers the store page if it does not come). Otherwise (an AppImage, Firefox) the usual flow: the
        // store page, or Firefox's extensions page.
        let by_itself = CHROME_STORE_ID.filter(|_| browser != Browser::Firefox).is_some_and(|id| self.browser_adds_it_itself(browser, id));
        if by_itself {
            match self.set(SetBrowserIntegration { browser, enabled: true }) {
                Ok(_) => {
                    tracing::info!(browser = ?browser, "browser connected on first start; it adds the extension from the Chrome Web Store by itself");
                    if self.contacts.last().is_none() {
                        if let Err(e) = self.contacts.start_waiting() {
                            tracing::warn!(error = %e, "could not remember that the app waits for the extension");
                        }
                    }
                }
                Err(e) => tracing::warn!(browser = ?browser, error = %e.message, "connecting the browser on first start failed"),
            }
        } else {
            match self.install_extension(InstallExtension { browser: Some(browser) }, resources) {
                Ok(r) => tracing::info!(browser = ?browser, method = r.method, "extension set up on first start"),
                Err(e) => tracing::warn!(browser = ?browser, error = %e.message, "setting up the extension on first start failed"),
            }
        }
        if let Err(e) = mark(&marker) {
            tracing::warn!(error = %e, "could not remember that the extension was offered");
        }
    }

    /// The browsers the user disconnected in Settings (`native-messaging/disconnected`, one name per line): the app
    /// never connects them again by itself.
    fn disconnected(&self) -> HashSet<String> {
        std::fs::read_to_string(self.manifest_dir.join("disconnected"))
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn remember_disconnected(&self, browser: Browser, disconnected: bool) -> std::io::Result<()> {
        let mut all = self.disconnected();
        let changed = if disconnected { all.insert(browser.as_str().to_string()) } else { all.remove(browser.as_str()) };
        if !changed {
            return Ok(());
        }
        let mut lines: Vec<String> = all.into_iter().collect();
        lines.sort();
        std::fs::create_dir_all(&self.manifest_dir)?;
        std::fs::write(self.manifest_dir.join("disconnected"), lines.join("\n"))
    }

    /// Every start: connect each browser installed on this computer (normal, snap or Flatpak package, or another
    /// browser of its line) that is not connected yet, so the extension added to it reaches the app with nothing to
    /// press. Never one the user disconnected. Release builds (debug builds only when a test asks), and it never
    /// fails the app: whatever goes wrong is logged, and Settings > Browser still has the buttons.
    pub fn connect_found_browsers(&self) {
        let wanted = !cfg!(debug_assertions) || dev_var("VELOX_AUTO_EXTENSION").is_some();
        if !wanted || !self.spec.host_exe.is_file() {
            return;
        }
        let platform = Platform::current();
        let disconnected = self.disconnected();
        let sandboxed = self.sandboxed();
        for (b, t) in &self.targets {
            let Some(t) = t else { continue };
            if disconnected.contains(b.as_str()) || nm::status(*b, &self.spec, &self.manifest_dir, t).registered {
                continue;
            }
            let found = launch::find_browser(*b, platform).is_some()
                || sandboxed.iter().any(|s| s.browser == *b)
                || !self.siblings(*b).is_empty();
            if !found {
                continue;
            }
            match self.set(SetBrowserIntegration { browser: *b, enabled: true }) {
                Ok(_) => tracing::info!(browser = ?b, "browser found on this computer: connected to the app"),
                Err(e) => tracing::warn!(browser = ?b, error = %e.message, "could not connect a browser found on this computer"),
            }
        }
    }

    /// Register or remove the host for one browser: from the user's buttons, and on its own only to connect (the
    /// first start's set-up, [`Self::connect_found_browsers`]), never a browser the user disconnected.
    pub fn set(&self, req: SetBrowserIntegration) -> Result<BrowserIntegrationDto, ApiError> {
        let target = self
            .targets
            .iter()
            .find(|(b, _)| *b == req.browser)
            .and_then(|(_, t)| t.as_ref())
            .ok_or_else(|| ApiError { code: "invalid_input", message: "this browser is not supported on this system".into() })?;
        let res = if req.enabled {
            nm::install(req.browser, &self.spec, &self.manifest_dir, target).and_then(|_| {
                self.install_siblings(req.browser);
                for b in self.sandboxed().iter().filter(|b| b.browser == req.browser) {
                    sandboxed::install(b, &self.spec)?;
                    self.serve_in(b);
                }
                Ok(())
            })
        } else {
            for b in self.sandboxed().iter().filter(|b| b.browser == req.browser) {
                if let Err(e) = sandboxed::uninstall(b) {
                    tracing::warn!(error = %e, package = b.package, "could not remove a sandboxed browser's registration");
                }
            }
            // Disconnect also stops the browser offering the store extension.
            if let Some(id) = CHROME_STORE_ID {
                if let Err(e) = store::withdraw(&self.store_way(req.browser, id)) {
                    tracing::warn!(error = %e, "could not withdraw the store extension offer");
                }
            }
            for t in self.siblings(req.browser) {
                if let Err(e) = nm::uninstall(req.browser, &self.spec, &self.manifest_dir, &t) {
                    tracing::warn!(error = %e, browser = ?req.browser, "could not disconnect another browser of this line");
                }
            }
            nm::uninstall(req.browser, &self.spec, &self.manifest_dir, target)
        };
        if res.is_ok() {
            if let Err(e) = self.remember_disconnected(req.browser, !req.enabled) {
                tracing::warn!(error = %e, "could not remember the browser's connection choice");
            }
        }
        res.map_err(|e| {
            tracing::warn!(error = %e, browser = ?req.browser, "browser registration failed");
            // No paths or OS error text to the UI.
            let message = if e.kind() == std::io::ErrorKind::NotFound {
                "the browser helper program is missing; reinstall VeloxFetch"
            } else {
                "could not change the browser registration"
            };
            ApiError { code: "engine", message: message.into() }
        })?;
        Ok(self.integration())
    }
}

/// Tests: the store pointer under a scratch home (macOS) or a scratch registry key (Windows), never the real one.
enum StoreScratch {
    Home(PathBuf),
    Key(String),
}

fn server_config() -> ServerConfig {
    ServerConfig { allow: Allowlist::default(), ..Default::default() }
}

/// Replace `dest` with a copy of the extension in `src`. Plain files only (no links), bounded size.
fn copy_extension(src: &Path, dest: &Path) -> std::io::Result<()> {
    fn walk(src: &Path, dest: &Path, depth: u32, files: &mut u32) -> std::io::Result<()> {
        if depth > 4 {
            return Err(std::io::Error::other("extension folder too deep"));
        }
        std::fs::create_dir_all(dest)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let to = dest.join(entry.file_name());
            if kind.is_dir() {
                walk(&entry.path(), &to, depth + 1, files)?;
            } else if kind.is_file() {
                *files += 1;
                if *files > 200 || entry.metadata()?.len() > 4 * 1024 * 1024 {
                    return Err(std::io::Error::other("extension folder too large"));
                }
                std::fs::copy(entry.path(), &to)?;
            }
        }
        Ok(())
    }
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    walk(src, dest, 0, &mut 0)
}

fn mark(marker: &Path) -> std::io::Result<()> {
    if let Some(dir) = marker.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(marker, b"1")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the window was told: each contact, and whether it was announced.
    type Told = Arc<Mutex<Vec<(ExtensionContact, bool)>>>;

    fn log(dir: &Path) -> (ContactLog, Told) {
        let told = Arc::new(Mutex::new(Vec::new()));
        let t = told.clone();
        let on = move |e| match e {
            ExtensionEvent::Contact { contact, announce } => t.lock().unwrap().push((contact, announce)),
            ExtensionEvent::Waiting { since } => assert!(since > 0),
        };
        (ContactLog::open(dir, Arc::new(on)), told)
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("velox-contact-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn the_first_contact_is_announced_saved_and_remembered_after_a_restart() {
        let dir = scratch("first");
        let (l, told) = log(&dir);
        assert_eq!(l.last(), None);
        let store = Caller::Chromium { extension_id: CHROME_STORE_ID.unwrap().into() };
        l.record(&store);
        l.record(&store); // pings come often: one save, one message
        let told = told.lock().unwrap().clone();
        assert_eq!(told.len(), 1);
        assert!(told[0].1, "the first contact ever is announced");
        assert!(told[0].0.from_store);
        let (again, told2) = log(&dir);
        assert_eq!(again.last(), Some(told[0].0), "kept in the data folder");
        again.record(&store);
        assert!(!told2.lock().unwrap()[0].1, "not announced again after a restart");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_awaited_extension_ends_the_wait_and_is_announced() {
        let dir = scratch("await");
        let (l, told) = log(&dir);
        l.record(&Caller::Chromium { extension_id: CHROME_EXTENSION_ID.into() });
        assert!(!told.lock().unwrap()[0].0.from_store, "the developer copy is not the store's");
        // Then the first start of a new installation waits for the store extension.
        l.start_waiting().unwrap();
        assert!(l.awaited_since().is_some_and(|t| t > 0));
        l.record(&Caller::Chromium { extension_id: CHROME_STORE_ID.unwrap().into() });
        let told = told.lock().unwrap().clone();
        assert_eq!(told.len(), 2, "a change of extension is saved at once");
        assert!(told[1].1 && told[1].0.from_store, "the awaited one is announced");
        assert_eq!(l.awaited_since(), None, "the wait is over");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_contact_file_is_ignored() {
        let dir = scratch("bad");
        std::fs::create_dir_all(dir.join("browser-extension")).unwrap();
        std::fs::write(dir.join("browser-extension/contact.json"), b"{not json").unwrap();
        std::fs::write(dir.join("browser-extension/.awaiting"), b"soon").unwrap();
        let (l, _) = log(&dir);
        assert_eq!(l.last(), None);
        assert_eq!(l.awaited_since(), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
