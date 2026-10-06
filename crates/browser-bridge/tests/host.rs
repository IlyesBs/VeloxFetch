//! The native host loop with in-memory "browser" streams and a real server behind it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use browser_bridge::frame::{read_frame, write_frame, MAX_BROWSER_MESSAGE, MAX_REPLY};
use browser_bridge::host::{run, HostConfig};
use browser_bridge::server::{BoxFuture, DownloadAccepted, Handler, HandlerError, ServerConfig};
use browser_bridge::{serve, Caller, DownloadRequest, Endpoint, Listener, Reply, Token, CHROME_EXTENSION_ID};
use serde_json::{json, Value};
use tokio::io::{duplex, AsyncWriteExt, DuplexStream};

static N: AtomicUsize = AtomicUsize::new(0);

fn unique(tag: &str) -> String {
    format!("velox-host-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst))
}

fn endpoint(name: &str) -> Endpoint {
    #[cfg(windows)]
    return Endpoint(format!(r"\\.\pipe\{name}"));
    #[cfg(not(windows))]
    return Endpoint(std::env::temp_dir().join(format!("{name}.sock")).to_string_lossy().to_string());
}

#[derive(Default)]
struct Recorder(Mutex<Vec<DownloadRequest>>);

impl Handler for Recorder {
    fn app_version(&self) -> String {
        "1.2.3".into()
    }
    fn download(&self, req: DownloadRequest, _c: Caller) -> BoxFuture<'_, Result<DownloadAccepted, HandlerError>> {
        Box::pin(async move {
            let mut v = self.0.lock().unwrap();
            v.push(req);
            Ok(DownloadAccepted { id: v.len() as i64, filename: "f.bin".into() })
        })
    }
}

struct App {
    handler: Arc<Recorder>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for App {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Start an "app": writes a fresh token file into `dir` and serves on `ep`.
fn start_app(ep: &Endpoint, dir: &std::path::Path) -> App {
    let token = Token::create_file(dir).unwrap();
    let handler = Arc::new(Recorder::default());
    let task = tokio::spawn(serve(Listener::bind(ep).unwrap(), token, handler.clone(), ServerConfig::default()));
    App { handler, task }
}

fn cfg(ep: &Endpoint, dir: &std::path::Path) -> HostConfig {
    HostConfig { endpoint: ep.clone(), data_dir: dir.to_path_buf(), app_exe: None, launch_wait: Duration::from_millis(300) }
}

fn caller() -> Caller {
    Caller::Chromium { extension_id: CHROME_EXTENSION_ID.into() }
}

/// A fake browser: the far ends of the host's stdin / stdout.
struct Browser {
    to_host: DuplexStream,
    from_host: DuplexStream,
    host: tokio::task::JoinHandle<()>,
}

fn spawn_host(cfg: HostConfig) -> Browser {
    let (to_host, mut host_in) = duplex(1 << 20);
    let (mut host_out, from_host) = duplex(1 << 20);
    let host = tokio::spawn(async move {
        run(&mut host_in, &mut host_out, caller(), &cfg).await.unwrap();
    });
    Browser { to_host, from_host, host }
}

impl Browser {
    async fn send_raw(&mut self, bytes: &[u8]) {
        write_frame(&mut self.to_host, bytes, MAX_BROWSER_MESSAGE).await.unwrap();
    }
    async fn ask(&mut self, msg: Value) -> Reply {
        self.send_raw(&serde_json::to_vec(&msg).unwrap()).await;
        let bytes = read_frame(&mut self.from_host, MAX_REPLY).await.unwrap().expect("a reply");
        serde_json::from_slice(&bytes).unwrap()
    }
}

fn dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(unique(tag));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn dl(url: &str) -> Value {
    json!({"command":"download","url":url,"source":"popup","cookies":"sid=1"})
}

#[tokio::test]
async fn messages_flow_browser_to_app_and_back() {
    let (ep, d) = (endpoint(&unique("flow")), dir("flow"));
    let app = start_app(&ep, &d);
    let mut b = spawn_host(cfg(&ep, &d));

    let pong = b.ask(json!({"command":"ping"})).await;
    assert!(pong.ok && pong.result.unwrap()["version"] == "1.2.3");
    let r = b.ask(dl("https://example.com/a.zip")).await;
    assert!(r.ok, "{r:?}");
    assert_eq!(r.result.unwrap()["id"], 1);
    assert_eq!(app.handler.0.lock().unwrap()[0].cookies.as_deref(), Some("sid=1"));

    drop(b.to_host); // the browser closes the port
    tokio::time::timeout(Duration::from_secs(2), b.host).await.expect("host exits when stdin closes").unwrap();
}

#[tokio::test]
async fn hostile_input_is_answered_with_errors_and_never_forwarded() {
    let (ep, d) = (endpoint(&unique("hostile")), dir("hostile"));
    let app = start_app(&ep, &d);
    let mut b = spawn_host(cfg(&ep, &d));
    for (msg, code) in [
        (json!({"command":"exec","cmd":"calc.exe"}), "unknown_command"),
        (json!({"command":"download","url":"file:///C:/Windows/win.ini","source":"popup"}), "invalid"),
        (json!({"command":"download","url":"javascript:alert(1)","source":"popup"}), "invalid"),
        (json!({"command":"download","url":"https://a.test/","source":"popup","path":"C:\\x"}), "bad_fields"),
        (json!([1, 2, 3]), "bad_shape"),
    ] {
        let r = b.ask(msg.clone()).await;
        assert_eq!(r.error.expect("error").code, code, "{msg}");
    }
    b.send_raw(b"{this is not json").await;
    let bytes = read_frame(&mut b.from_host, MAX_REPLY).await.unwrap().unwrap();
    assert_eq!(serde_json::from_slice::<Reply>(&bytes).unwrap().error.unwrap().code, "bad_json");
    assert!(app.handler.0.lock().unwrap().is_empty(), "nothing hostile reached the app");
    assert!(b.ask(json!({"command":"ping"})).await.ok, "and the session is still fine");
}

#[tokio::test]
async fn an_oversized_message_ends_the_session_without_reading_it() {
    let (ep, d) = (endpoint(&unique("big")), dir("big"));
    let _app = start_app(&ep, &d);
    let mut b = spawn_host(cfg(&ep, &d));
    // A header promising ~1 GiB; the host must not wait for (or buffer) the body.
    b.to_host.write_all(&(1u32 << 30).to_ne_bytes()).await.unwrap();
    let bytes = read_frame(&mut b.from_host, MAX_REPLY).await.unwrap().unwrap();
    assert_eq!(serde_json::from_slice::<Reply>(&bytes).unwrap().error.unwrap().code, "too_large");
    tokio::time::timeout(Duration::from_secs(2), b.host).await.expect("host ends the session").unwrap();
}

#[tokio::test]
async fn a_message_just_under_the_limit_is_handled() {
    let (ep, d) = (endpoint(&unique("edge")), dir("edge"));
    let _app = start_app(&ep, &d);
    let mut b = spawn_host(cfg(&ep, &d));
    // Valid JSON of nearly the maximum size; fails validation (unknown fields), but is *read*.
    let pad = "x".repeat(MAX_BROWSER_MESSAGE - 64);
    let r = b.ask(json!({"command":"ping","pad":pad})).await;
    assert_eq!(r.error.unwrap().code, "bad_fields");
}

#[tokio::test]
async fn without_the_app_the_answer_is_a_clear_error_and_nothing_is_launched_for_a_ping() {
    let (ep, d) = (endpoint(&unique("noapp")), dir("noapp"));
    let mut b = spawn_host(cfg(&ep, &d));
    let r = b.ask(json!({"command":"ping"})).await;
    assert_eq!(r.error.unwrap().code, "app_not_running");
    let r = b.ask(dl("https://example.com/a")).await;
    assert_eq!(r.error.unwrap().code, "app_not_running", "no app_exe configured: never starts anything");
}

#[cfg(windows)]
#[tokio::test]
async fn the_app_is_started_for_a_download_but_never_for_a_ping() {
    let (ep, d) = (endpoint(&unique("launch")), dir("launch"));
    let marker = d.join("launched.txt");
    let script = d.join("fake-app.cmd");
    std::fs::write(&script, format!("@echo off\r\necho started> \"{}\"\r\n", marker.display())).unwrap();
    let mut c = cfg(&ep, &d);
    c.app_exe = Some(script);
    let mut b = spawn_host(c);

    let r = b.ask(json!({"command":"ping"})).await;
    assert_eq!(r.error.unwrap().code, "app_not_running");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!marker.exists(), "a ping must never start the app");

    let r = b.ask(dl("https://example.com/a")).await;
    assert_eq!(r.error.unwrap().code, "app_not_running", "the fake app never opens the endpoint, so this times out");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(marker.exists(), "a download request starts the app");
}

/// An "app" on its own runtime: dropping it tears down every task and pipe handle, like a
/// process exit (aborting only the accept loop would leave connection tasks, and the pipe, alive).
struct Process {
    rt: Option<tokio::runtime::Runtime>,
}

impl Process {
    fn start(ep: &Endpoint, dir: &std::path::Path) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let token = Token::create_file(dir).unwrap();
        let ep = ep.clone();
        rt.spawn(async move {
            let listener = Listener::bind(&ep).unwrap();
            serve(listener, token, Arc::new(Recorder::default()), ServerConfig::default()).await;
        });
        std::thread::sleep(Duration::from_millis(150));
        Self { rt: Some(rt) }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

#[tokio::test]
async fn an_app_restart_is_survived_because_the_token_is_re_read() {
    let (ep, d) = (endpoint(&unique("restart")), dir("restart"));
    let app = Process::start(&ep, &d);
    let mut b = spawn_host(cfg(&ep, &d));
    assert!(b.ask(json!({"command":"ping"})).await.ok);

    drop(app); // the app quits ...
    tokio::time::sleep(Duration::from_millis(300)).await;
    let _app2 = Process::start(&ep, &d); // ... and starts again, with a new token
    let r = b.ask(dl("https://example.com/again")).await;
    assert!(r.ok, "{r:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_browser_in_a_sandbox_reaches_the_same_app_through_its_own_folder() {
    // The app serves its usual link and, with the same token, a second one in the browser's own folder
    // (what the desktop bridge does for snap / Flatpak browsers).
    let (main_ep, main_dir) = (endpoint(&unique("main")), dir("main"));
    let app = start_app(&main_ep, &main_dir);
    let token = Token::read_file(&main_dir).unwrap();
    let sandbox = dir("snap-common").join("app.velox.fetch");
    token.write_file(&sandbox).unwrap();
    let sandbox_ep = Endpoint::default_for(&sandbox);
    let second = tokio::spawn(serve(Listener::bind(&sandbox_ep).unwrap(), token, app.handler.clone(), ServerConfig::default()));

    // The helper inside the sandbox only sees that folder.
    let mut b = spawn_host(cfg(&sandbox_ep, &sandbox));
    let r = b.ask(dl("https://example.com/sandboxed.zip")).await;
    assert!(r.ok, "{r:?}");
    assert_eq!(app.handler.0.lock().unwrap()[0].url, "https://example.com/sandboxed.zip");
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(sandbox.join("bridge.sock")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the sandbox link is private to the user");
    }
    second.abort();
}
