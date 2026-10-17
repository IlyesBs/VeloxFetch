//! The host <-> app link over a real local pipe / socket: authentication in both directions,
//! caller allowlist, size and rate limits, hostile peers.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use browser_bridge::frame::{read_frame, write_frame, MAX_IPC_MESSAGE};
use browser_bridge::server::{BoxFuture, DownloadAccepted, Handler, HandlerError, ServerConfig};
use browser_bridge::transport::connect;
use browser_bridge::{
    serve, Caller, Client, ClientError, DownloadRequest, Endpoint, Listener, Token, CHROME_EXTENSION_ID, CHROME_STORE_ID,
    FIREFOX_EXTENSION_ID,
};
use serde_json::{json, Value};

static N: AtomicUsize = AtomicUsize::new(0);

fn endpoint() -> Endpoint {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let name = format!("velox-test-{}-{n}", std::process::id());
    #[cfg(windows)]
    return Endpoint(format!(r"\\.\pipe\{name}"));
    #[cfg(not(windows))]
    return Endpoint(std::env::temp_dir().join(format!("{name}.sock")).to_string_lossy().to_string());
}

#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<(DownloadRequest, Caller)>>,
    fail_with: Mutex<Option<&'static str>>,
    contacts: Mutex<Vec<Caller>>,
}

impl Handler for Recorder {
    fn app_version(&self) -> String {
        "9.9.9".into()
    }

    fn download(&self, req: DownloadRequest, caller: Caller) -> BoxFuture<'_, Result<DownloadAccepted, HandlerError>> {
        Box::pin(async move {
            if let Some(code) = *self.fail_with.lock().unwrap() {
                return Err(HandlerError { code, message: "refused by the app".into() });
            }
            let mut seen = self.seen.lock().unwrap();
            seen.push((req.clone(), caller));
            Ok(DownloadAccepted { id: seen.len() as i64, filename: req.filename.unwrap_or_else(|| "from-url.bin".into()) })
        })
    }

    fn contacted(&self, caller: &Caller) {
        self.contacts.lock().unwrap().push(caller.clone());
    }
}

struct Rig {
    ep: Endpoint,
    token: Token,
    handler: Arc<Recorder>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn start(cfg: ServerConfig) -> Rig {
    let ep = endpoint();
    let token = Token::generate();
    let handler = Arc::new(Recorder::default());
    let listener = Listener::bind(&ep).unwrap();
    let task = tokio::spawn(serve(listener, token.clone(), handler.clone(), cfg));
    Rig { ep, token, handler, task }
}

fn chrome() -> Caller {
    Caller::Chromium { extension_id: CHROME_EXTENSION_ID.into() }
}

fn dl(url: &str) -> Value {
    json!({"command":"download","url":url,"source":"contextMenu"})
}

#[tokio::test]
async fn ping_and_download_round_trip_with_the_caller_identity() {
    let rig = start(ServerConfig::default());
    let mut c = Client::connect(&rig.ep, &rig.token, &chrome()).await.unwrap();

    let pong = c.request(json!({"command":"ping"})).await.unwrap();
    assert!(pong.ok);
    let result = pong.result.unwrap();
    assert_eq!(result["version"], "9.9.9");
    assert_eq!(result["videoBar"], true, "the ping tells the extension whether to show the video bar");

    let mut v = dl("https://example.com/a.zip");
    v["filename"] = json!("a.zip");
    v["referrer"] = json!("https://example.com/page");
    let r = c.request(v).await.unwrap();
    assert!(r.ok, "{r:?}");
    assert_eq!(r.result.unwrap()["filename"], "a.zip");

    let seen = rig.handler.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0.referrer.as_deref(), Some("https://example.com/page"));
    assert_eq!(seen[0].1, chrome());
}

#[tokio::test]
async fn the_app_learns_each_contact_of_an_allowed_extension_and_nothing_else() {
    let rig = start(ServerConfig::default());
    let store = Caller::Chromium { extension_id: CHROME_STORE_ID.unwrap().into() };
    let mut c = Client::connect(&rig.ep, &rig.token, &store).await.unwrap();
    assert!(c.request(json!({"command":"ping"})).await.unwrap().ok);
    assert_eq!(*rig.handler.contacts.lock().unwrap(), vec![store.clone()], "one contact per connection, after the handshake");
    // A wrong token or a caller outside the allowlist is not a contact.
    assert!(Client::connect(&rig.ep, &Token::generate(), &store).await.is_err());
    let evil = Caller::Chromium { extension_id: "abcdefghijklmnopabcdefghijklmnop".into() };
    assert!(Client::connect(&rig.ep, &rig.token, &evil).await.is_err());
    assert_eq!(rig.handler.contacts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn firefox_callers_are_accepted_too() {
    let rig = start(ServerConfig::default());
    let ff = Caller::Firefox { extension_id: FIREFOX_EXTENSION_ID.into() };
    let mut c = Client::connect(&rig.ep, &rig.token, &ff).await.unwrap();
    assert!(c.request(dl("https://example.com/b.bin")).await.unwrap().ok);
    assert_eq!(rig.handler.seen.lock().unwrap()[0].1, ff);
}

#[tokio::test]
async fn a_client_with_the_wrong_token_is_refused_and_the_server_keeps_working() {
    let rig = start(ServerConfig::default());
    let wrong = Token::generate();
    assert!(matches!(Client::connect(&rig.ep, &wrong, &chrome()).await, Err(ClientError::AuthFailed)));
    // Nothing reached the handler, and a legitimate client still gets in afterwards.
    assert!(rig.handler.seen.lock().unwrap().is_empty());
    let mut c = Client::connect(&rig.ep, &rig.token, &chrome()).await.unwrap();
    assert!(c.request(json!({"command":"ping"})).await.unwrap().ok);
}

#[tokio::test]
async fn the_client_refuses_an_impostor_server() {
    // A different process squatting on the pipe name does not know the token.
    let rig = start(ServerConfig::default());
    let real_token_of_the_host = Token::generate(); // what the host read from the real token file
    let r = Client::connect(&rig.ep, &real_token_of_the_host, &chrome()).await;
    assert!(matches!(r, Err(ClientError::AuthFailed)), "host and server disagree on the token: neither trusts the other");
}

#[tokio::test]
async fn callers_outside_the_allowlist_are_refused_even_with_the_right_token() {
    let rig = start(ServerConfig::default());
    for evil in [
        Caller::Chromium { extension_id: "abcdefghijklmnopabcdefghijklmnop".into() },
        Caller::Firefox { extension_id: "evil@evil.test".into() },
    ] {
        assert!(matches!(Client::connect(&rig.ep, &rig.token, &evil).await, Err(ClientError::AuthFailed)));
    }
    assert!(rig.handler.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn hostile_messages_get_error_replies_and_never_reach_the_handler() {
    let rig = start(ServerConfig::default());
    let mut c = Client::connect(&rig.ep, &rig.token, &chrome()).await.unwrap();
    let cases = [
        (json!({"command":"exec","cmd":"calc.exe"}), "unknown_command"),
        (json!({"command":"readFile","path":"C:\\Windows\\win.ini"}), "unknown_command"),
        (json!({"command":"ping","x":1}), "bad_fields"),
        (dl("file:///C:/Windows/win.ini"), "invalid"),
        (dl("javascript:alert(1)"), "invalid"),
        (json!({"command":"download","url":"https://a.test/","source":"contextMenu","destination":"C:\\Windows"}), "bad_fields"),
        (json!("just a string"), "bad_shape"),
        (json!(["download"]), "bad_shape"),
    ];
    for (msg, code) in cases {
        let r = c.request(msg.clone()).await.unwrap();
        assert!(!r.ok, "{msg}");
        assert_eq!(r.error.unwrap().code, code, "{msg}");
    }
    assert!(rig.handler.seen.lock().unwrap().is_empty());
    // The connection is still healthy after all that.
    assert!(c.request(json!({"command":"ping"})).await.unwrap().ok);
}

#[tokio::test]
async fn handler_errors_are_passed_on_as_codes() {
    let rig = start(ServerConfig::default());
    *rig.handler.fail_with.lock().unwrap() = Some("invalid_input");
    let mut c = Client::connect(&rig.ep, &rig.token, &chrome()).await.unwrap();
    let r = c.request(dl("https://example.com/a")).await.unwrap();
    assert_eq!(r.error.unwrap().code, "invalid_input");
}

#[tokio::test]
async fn oversized_and_garbage_frames_close_the_connection_and_the_server_survives() {
    let rig = start(ServerConfig::default());
    // 1. After the handshake, a frame header claiming 1 GiB.
    let mut c = Client::connect(&rig.ep, &rig.token, &chrome()).await.unwrap();
    let huge = json!({"id":1,"message":{"command":"ping","pad":"x".repeat(MAX_IPC_MESSAGE)}});
    let r = c.request(huge).await;
    assert!(r.is_err(), "an oversized request must not be answered");

    // 2. A raw peer that sends garbage instead of a handshake.
    let mut raw = connect(&rig.ep).await.unwrap();
    let _hello = read_frame(&mut raw, MAX_IPC_MESSAGE).await.unwrap().unwrap();
    write_frame(&mut raw, b"this is not json", MAX_IPC_MESSAGE).await.unwrap();
    assert!(read_frame(&mut raw, MAX_IPC_MESSAGE).await.map(|f| f.is_none()).unwrap_or(true), "server hangs up");

    // 3. A raw peer that claims a 4 GiB frame right away.
    let mut raw = connect(&rig.ep).await.unwrap();
    let _hello = read_frame(&mut raw, MAX_IPC_MESSAGE).await.unwrap().unwrap();
    use tokio::io::AsyncWriteExt;
    raw.write_all(&u32::MAX.to_ne_bytes()).await.unwrap();
    assert!(read_frame(&mut raw, MAX_IPC_MESSAGE).await.map(|f| f.is_none()).unwrap_or(true));

    // Still serving legitimate clients.
    let mut ok = Client::connect(&rig.ep, &rig.token, &chrome()).await.unwrap();
    assert!(ok.request(json!({"command":"ping"})).await.unwrap().ok);
}

#[tokio::test]
async fn requests_are_rate_limited_per_connection() {
    let rig = start(ServerConfig { rate_limit: (5, Duration::from_secs(60)), ..Default::default() });
    let mut c = Client::connect(&rig.ep, &rig.token, &chrome()).await.unwrap();
    let mut limited = 0;
    for _ in 0..12 {
        let r = c.request(json!({"command":"ping"})).await.unwrap();
        if !r.ok && r.error.as_ref().unwrap().code == "rate_limited" {
            limited += 1;
        }
    }
    assert_eq!(limited, 7, "5 allowed, the other 7 refused");
    // A fresh connection has its own budget.
    let mut c2 = Client::connect(&rig.ep, &rig.token, &chrome()).await.unwrap();
    assert!(c2.request(json!({"command":"ping"})).await.unwrap().ok);
}

#[tokio::test]
async fn silent_peers_time_out_and_do_not_hold_connection_slots_forever() {
    let rig = start(ServerConfig { max_connections: 1, handshake_timeout: Duration::from_millis(300), ..Default::default() });
    let mut squatter = connect(&rig.ep).await.unwrap(); // takes the only slot and says nothing
    let _ = read_frame(&mut squatter, MAX_IPC_MESSAGE).await;
    tokio::time::sleep(Duration::from_millis(600)).await; // handshake timeout frees the slot
    let mut c = Client::connect(&rig.ep, &rig.token, &chrome()).await.unwrap();
    assert!(c.request(json!({"command":"ping"})).await.unwrap().ok);
}

#[tokio::test]
async fn only_one_process_can_own_the_endpoint() {
    let rig = start(ServerConfig::default());
    #[cfg(windows)]
    assert!(Listener::bind(&rig.ep).is_err(), "a second owner of the pipe name must be refused (no squatting)");
    #[cfg(not(windows))]
    let _ = &rig; // Unix sockets are protected by 0600 permissions instead
}

#[tokio::test]
async fn nobody_is_home() {
    let ep = endpoint(); // nothing listening
    let r = Client::connect(&ep, &Token::generate(), &chrome()).await;
    assert!(matches!(r, Err(ClientError::Unavailable)));
}

#[tokio::test]
async fn a_second_instance_cannot_take_the_link_and_is_told_why() {
    let ep = endpoint();
    let first = Listener::bind(&ep).unwrap();
    let second = Listener::bind(&ep).err().expect("the link already has an owner");
    assert!(browser_bridge::owned_by_another_instance(&second), "{second:?}");
    // The first instance still owns it: a client reaches it.
    let accept = tokio::spawn(async move {
        let mut first = first;
        first.accept().await.map(|_| ())
    });
    connect(&ep).await.expect("still reachable");
    tokio::time::timeout(Duration::from_secs(5), accept).await.unwrap().unwrap().unwrap();
}

/// The very first start after installing: the data folder does not exist yet when the link is opened. It must
/// still be opened (otherwise browser integration and the one-copy guard are off for that whole run).
#[cfg(unix)]
#[tokio::test]
async fn the_link_opens_on_the_very_first_start_before_the_data_folder_exists() {
    let dir = std::env::temp_dir().join(format!("velox-first-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ep = Endpoint::default_for(&dir.join("app.velox.fetch"));
    let first = Listener::bind(&ep).expect("the folder is made");
    let second = Listener::bind(&ep).err().expect("and the link is owned from then on");
    assert!(browser_bridge::owned_by_another_instance(&second));
    drop(first);
    let _ = std::fs::remove_dir_all(dir);
}
