//! The app's side of the browser integration: serves the local link the native host talks to, turns
//! validated handoffs into downloads, and lets the user register / remove the host with browsers.
//!
//! Nothing here decides what is safe: `browser-bridge` validated the message twice already, and the
//! download manager validates the URL, file name and headers again before anything is stored.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use browser_bridge::launch as app_start;
use browser_bridge::server::{BoxFuture, DownloadAccepted, Handler, HandlerError, ServerConfig};
use browser_bridge::{
    chromium_ids, serve, Allowlist, Caller, DownloadRequest, Endpoint, Listener, MediaHint, Token, CHROME_EXTENSION_ID, CHROME_STORE_ID,
    FIREFOX_EXTENSION_ID, HOST_NAME,
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
}

impl Handler for AppHandler {
    fn app_version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    fn video_bar(&self) -> bool {
        self.manager.video_bar()
    }

    fn download(&self, req: DownloadRequest, caller: Caller) -> BoxFuture<'_, Result<DownloadAccepted, HandlerError>> {
        Box::pin(async move {
            tracing::info!(origin = %caller.origin(), source = ?req.source, media = ?req.media, "download handed over by a browser");
            let stream = url::Url::parse(&req.url).is_ok_and(|u| download_manager::is_stream_link(&u));
            let media = req.media.or(stream.then_some(MediaHint::Stream));
            let new = NewDownload { url: req.url, filename: req.filename, ..Default::default() };
            let ctx = BrowserContext { referrer: req.referrer, cookies: req.cookies, user_agent: req.user_agent, incognito: req.incognito };
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
    /// Folder holding the extension, to be picked in the browser (Load unpacked / temporary add-on).
    pub folder: String,
    /// The browser was found and opened on its extensions page.
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
    pub firefox_extension_id: String,
    /// Ask the user (a dialog with the file's details) before a browser download starts.
    pub confirm_downloads: bool,
    /// Show the "Download video" bar over videos in the browser.
    pub video_bar: bool,
    /// Cookies and credentials handed over by a browser survive a restart, sealed with a key from
    /// the operating system's credential store. When false they are kept in memory only.
    pub secrets_protected: bool,
}

pub struct Bridge {
    manager: DownloadManager,
    resource_dir: Option<PathBuf>,
    link_running: bool,
    manifest_dir: PathBuf,
    spec: HostSpec,
    targets: Vec<(Browser, Option<Target>)>,
    home: Option<PathBuf>,
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
    ) -> Self {
        let link = match listener {
            None => None,
            Some(listener) => match Token::create_file(data_dir) {
                Err(e) => {
                    tracing::warn!(error = %e, "browser bridge: cannot write the token file; browser integration is off");
                    None
                }
                Ok(token) => {
                    let handler = Arc::new(AppHandler { manager: manager.clone(), notify });
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
        let bridge = Self {
            manager,
            resource_dir,
            link_running,
            manifest_dir: data_dir.join("native-messaging"),
            spec,
            targets,
            home,
            sandbox_roots,
            link,
            served: Mutex::new(HashSet::new()),
            start_link: app
                .filter(|_| !cfg!(debug_assertions) && Platform::current() == Platform::Linux)
                .map(|exe| ProtocolHandler { scheme: app_start::SCHEME.into(), exe }),
        };
        bridge.refresh_sandboxed();
        bridge
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
            firefox_extension_id: FIREFOX_EXTENSION_ID.to_string(),
            confirm_downloads: self.manager.browser_confirm(),
            video_bar: self.manager.video_bar(),
            secrets_protected: self.manager.secrets_protected(),
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

        // 2. published in a store: let the browser offer it (its own prompt; the store page opens too)
        let launching = dev_var("VELOX_NO_LAUNCH").is_none();
        if let Some(id) = CHROME_STORE_ID {
            let way = store::support(browser, platform, self.home.as_deref(), id);
            match store::offer(&way) {
                Ok(true) => {
                    let opened_browser =
                        launching && launch::find_browser(browser, platform).is_some_and(|l| launch::open_store_page(&l, id).is_ok());
                    return Ok(ExtensionInstallDto {
                        browser,
                        method: "store",
                        folder: String::new(),
                        opened_browser,
                        opened_folder: false,
                        integration: self.integration(),
                    });
                }
                Ok(false) => {} // no per-user way here (Linux): Developer mode below
                Err(e) => tracing::warn!(error = %e, "offering the store extension failed; using Developer mode"),
            }
        }

        // 3. the extension files, in a fixed folder inside the app's data folder
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

        // 4. open the browser on its extensions page, and the folder to pick. Tests set
        // VELOX_NO_LAUNCH so they never start real programs (debug builds only).
        let opened_browser =
            launching && launch::find_browser(browser, platform).is_some_and(|l| launch::open_extensions_page(browser, &l).is_ok());
        let opened_folder = launching && launch::open_folder(platform, &folder).is_ok();

        Ok(ExtensionInstallDto {
            browser,
            method: "developer",
            folder: folder.to_string_lossy().into_owned(),
            opened_browser,
            opened_folder,
            integration: self.integration(),
        })
    }

    /// First start only: set the extension up for the default browser without waiting for the user to find the
    /// button (connect the browser, then the store prompt or the extensions page). Done once per installation (a
    /// marker in the data folder), never in debug builds unless a test asks, and it never fails the app: whatever
    /// goes wrong is logged, and Settings > Browser still has the button.
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
        match self.install_extension(InstallExtension { browser: Some(browser) }, resources) {
            Ok(r) => tracing::info!(browser = ?browser, method = r.method, "extension set up on first start"),
            Err(e) => tracing::warn!(browser = ?browser, error = %e.message, "setting up the extension on first start failed"),
        }
        if let Err(e) = mark(&marker) {
            tracing::warn!(error = %e, "could not remember that the extension was offered");
        }
    }

    /// Register or remove the host for one browser. Only ever called from an explicit user action.
    pub fn set(&self, req: SetBrowserIntegration) -> Result<BrowserIntegrationDto, ApiError> {
        let target = self
            .targets
            .iter()
            .find(|(b, _)| *b == req.browser)
            .and_then(|(_, t)| t.as_ref())
            .ok_or_else(|| ApiError { code: "invalid_input", message: "this browser is not supported on this system".into() })?;
        let res = if req.enabled {
            nm::install(req.browser, &self.spec, &self.manifest_dir, target).and_then(|_| {
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
                let way = store::support(req.browser, Platform::current(), self.home.as_deref(), id);
                if let Err(e) = store::withdraw(&way) {
                    tracing::warn!(error = %e, "could not withdraw the store extension offer");
                }
            }
            nm::uninstall(req.browser, &self.spec, &self.manifest_dir, target)
        };
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
