//! Updates of VeloxFetch itself (Phase 19), against a local release server: only a manifest signed with a
//! trusted key is believed, never an older one than already seen, and a package is kept (and handed over for
//! installing) only when its size and SHA-256 match that signed manifest.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use download_manager::{DownloadManager, ManagerConfig, UpdateConfig};
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use update_verify::{key_id, to_hex, Policy, TrustedKeys, Version, KEY_PREFIX, SIGNING_CONTEXT, SIG_PREFIX};
use velox_integration_tests::{random_data, serve, Server};

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("velox-update-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The installation kind and package name used on this system.
fn kind() -> (&'static str, &'static str) {
    match std::env::consts::OS {
        "windows" => ("nsis", "VeloxFetch_2.0.0_x64-setup.exe"),
        "macos" => ("dmg", "VeloxFetch_2.0.0_aarch64.dmg"),
        _ => ("deb", "VeloxFetch_2.0.0_amd64.deb"),
    }
}

fn signer() -> SigningKey {
    SigningKey::from_bytes(&[42u8; 32])
}

fn sign(k: &SigningKey, manifest: &[u8]) -> String {
    let mut m = SIGNING_CONTEXT.to_vec();
    m.extend_from_slice(manifest);
    format!("{SIG_PREFIX} {} {}\n", to_hex(&key_id(k.verifying_key().as_bytes())), to_hex(&k.sign(&m).to_bytes()))
}

struct Release {
    server: Server,
    data: Arc<Vec<u8>>,
}

impl Release {
    async fn new() -> Self {
        let data = random_data(300_000, 7);
        Release { server: serve(data.clone()).await, data }
    }

    fn prefix(&self) -> String {
        self.server.url("/rel/")
    }

    /// Publish a manifest for `version` (released at `released`) whose package is `self.data`, signed by `k`.
    fn publish(&self, version: &str, released: u64, k: &SigningKey) {
        let (kind, name) = kind();
        let target = update_verify::target_for(kind).unwrap();
        let manifest = serde_json::json!({
            "schema": 1, "product": "VeloxFetch", "version": version, "released": released,
            "packages": [{"target": target, "name": name, "url": format!("{}v{version}/{name}", self.prefix()),
                          "size": self.data.len(), "sha256": to_hex(&Sha256::digest(&self.data[..]))}]
        })
        .to_string();
        self.server.put("/feed/latest.json", manifest.clone());
        self.server.put("/feed/latest.json.sig", sign(k, manifest.as_bytes()));
    }

    async fn manager(&self, dir: &Path) -> DownloadManager {
        let mut cfg = ManagerConfig::new(dir.join("state.db"), dir.join("dl"));
        let keys = TrustedKeys::parse(&format!("{KEY_PREFIX} {}\n", to_hex(signer().verifying_key().as_bytes()))).unwrap();
        cfg.update = Some(UpdateConfig {
            keys,
            policy: Policy { product: "VeloxFetch".into(), url_prefix: self.prefix(), allow_loopback_http: true },
            feed: self.server.url("/feed/latest.json"),
            release_page: "https://example.invalid/releases".into(),
            latest_release: None,
            current: Version(1, 0, 0),
            kind: Some(kind().0.into()),
            allow_loopback_http: true,
            first_check: Duration::from_secs(3600),
            every: Duration::from_secs(3600),
            dir: Some(dir.join("updates")),
        });
        DownloadManager::new(cfg).await.unwrap()
    }
}

#[tokio::test]
async fn a_signed_newer_version_is_found_downloaded_verified_and_handed_over() {
    let rel = Release::new().await;
    rel.publish("2.0.0", 1000, &signer());
    let dir = temp("ok");
    let m = rel.manager(&dir).await;
    assert!(m.update_status().supported);
    let s = m.check_for_updates().await;
    assert_eq!((s.phase, s.version.as_deref(), s.size), ("available", Some("2.0.0"), Some(rel.data.len() as u64)));
    let s = m.download_update().await.unwrap();
    assert_eq!(s.phase, "ready", "{s:?}");
    let plan = m.prepare_update_install().await.unwrap();
    assert_eq!((plan.version.as_str(), plan.kind.as_str()), ("2.0.0", kind().0));
    assert_eq!(std::fs::read(&plan.path).unwrap(), *rel.data);
    assert_eq!(plan.path, dir.join("updates").join(kind().1));
    // Nothing else is left in the folder.
    assert_eq!(std::fs::read_dir(dir.join("updates")).unwrap().count(), 1);
    // A check while it is ready does not throw the verified package away.
    assert_eq!(m.check_for_updates().await.phase, "ready");
    // Changed on disk after it was verified: refused, and back to "download it again".
    std::fs::write(&plan.path, b"replaced by something else").unwrap();
    assert!(m.prepare_update_install().await.is_err());
    let s = m.update_status();
    assert_eq!((s.phase, s.error), ("available", Some("corrupt")));
    assert!(!plan.path.exists());
    m.shutdown().await;
}

#[tokio::test]
async fn a_package_that_does_not_match_the_signed_checksum_is_never_kept() {
    let rel = Release::new().await;
    rel.publish("2.0.0", 1000, &signer());
    // The server now sends other bytes of the same length (a tampered mirror).
    rel.server.set_data(random_data(rel.data.len(), 8));
    let dir = temp("tampered");
    let m = rel.manager(&dir).await;
    assert_eq!(m.check_for_updates().await.phase, "available");
    let s = m.download_update().await.unwrap();
    assert_eq!((s.phase, s.error), ("available", Some("corrupt")));
    assert!(m.prepare_update_install().await.is_err());
    let left: Vec<_> = std::fs::read_dir(dir.join("updates")).unwrap().flatten().map(|e| e.file_name()).collect();
    assert!(left.is_empty(), "{left:?}");
    m.shutdown().await;
}

#[tokio::test]
async fn a_longer_package_is_cut_off_at_the_signed_size() {
    let rel = Release::new().await;
    rel.publish("2.0.0", 1000, &signer());
    // A server that ignores ranges and sends the package with more bytes after it.
    let mut longer = (*rel.data).clone();
    longer.extend_from_slice(&[0u8; 50_000]);
    rel.server.put(&format!("/rel/v2.0.0/{}", kind().1), longer);
    let dir = temp("longer");
    let m = rel.manager(&dir).await;
    m.check_for_updates().await;
    let s = m.download_update().await.unwrap();
    assert_eq!((s.phase, s.error), ("available", Some("corrupt")));
    // Nothing past the signed size was ever written, and nothing is kept.
    assert_eq!(std::fs::read_dir(dir.join("updates")).unwrap().count(), 0);
    m.shutdown().await;
}

#[tokio::test]
async fn an_unsigned_or_foreign_signed_manifest_is_refused() {
    let rel = Release::new().await;
    rel.publish("2.0.0", 1000, &SigningKey::from_bytes(&[9u8; 32]));
    let dir = temp("foreign");
    let m = rel.manager(&dir).await;
    let s = m.check_for_updates().await;
    assert_eq!((s.phase, s.error, s.version), ("failed", Some("signature"), None));
    assert!(m.download_update().await.is_err());
    // A correct signature over another manifest.
    rel.publish("2.0.0", 1000, &signer());
    let sig = String::from_utf8(rel.server.state.routes.lock().unwrap()["/feed/latest.json.sig"].to_vec()).unwrap();
    rel.publish("3.0.0", 1000, &signer());
    rel.server.put("/feed/latest.json.sig", sig);
    assert_eq!(m.check_for_updates().await.error, Some("signature"));
    // No signature at all (404).
    rel.server.state.routes.lock().unwrap().remove("/feed/latest.json.sig");
    let s = m.check_for_updates().await;
    assert_eq!(s.phase, "failed");
    m.shutdown().await;
}

#[tokio::test]
async fn never_an_older_version_and_never_an_older_manifest() {
    let rel = Release::new().await;
    let dir = temp("rollback");
    let m = rel.manager(&dir).await;
    // The same version, or an older one: up to date (never "updated" backwards).
    rel.publish("1.0.0", 1000, &signer());
    assert_eq!(m.check_for_updates().await.phase, "upToDate");
    rel.publish("0.9.0", 1001, &signer());
    assert_eq!(m.check_for_updates().await.phase, "upToDate");
    rel.publish("2.0.0", 2000, &signer());
    assert_eq!(m.check_for_updates().await.phase, "available");
    // An older (genuinely signed) manifest replayed later: refused, not taken as "up to date".
    rel.publish("1.0.0", 1500, &signer());
    let s = m.check_for_updates().await;
    assert_eq!((s.phase, s.error), ("failed", Some("stale")));
    m.shutdown().await;
    // It is remembered across restarts.
    let m = rel.manager(&dir).await;
    assert_eq!(m.check_for_updates().await.error, Some("stale"));
    m.shutdown().await;
}

#[tokio::test]
async fn an_interrupted_download_continues_and_is_still_verified() {
    let rel = Release::new().await;
    rel.publish("2.0.0", 1000, &signer());
    let dir = temp("resume");
    // A third of the package from an earlier run.
    std::fs::create_dir_all(dir.join("updates")).unwrap();
    std::fs::write(dir.join("updates").join(format!("{}.part", kind().1)), &rel.data[..100_000]).unwrap();
    let m = rel.manager(&dir).await;
    m.check_for_updates().await;
    rel.server.state.request_heads.lock().unwrap().clear();
    assert_eq!(m.download_update().await.unwrap().phase, "ready");
    let heads = rel.server.state.request_heads.lock().unwrap().join("\n");
    assert!(heads.contains("range: bytes=100000-"), "{heads}");
    // No cookie, credential or identifying header is ever sent with update requests.
    assert!(!heads.contains("cookie:") && !heads.contains("authorization:"), "{heads}");
    assert_eq!(std::fs::read(m.prepare_update_install().await.unwrap().path).unwrap(), *rel.data);
    m.shutdown().await;
}

#[tokio::test]
async fn automatic_checks_follow_the_setting_and_a_copy_without_keys_never_checks() {
    let rel = Release::new().await;
    rel.publish("2.0.0", 1000, &signer());
    let dir = temp("auto");
    let m = rel.manager(&dir).await;
    assert!(m.update_status().auto_check);
    assert!(!m.set_update_auto(false).await.unwrap().auto_check);
    m.shutdown().await;
    let m = rel.manager(&dir).await;
    assert!(!m.update_status().auto_check, "the choice is kept");
    m.shutdown().await;

    let dir = temp("nokeys");
    let mut cfg = ManagerConfig::new(dir.join("state.db"), dir.join("dl"));
    cfg.update = None;
    let m = DownloadManager::new(cfg).await.unwrap();
    let hits = rel.server.state.request_heads.lock().unwrap().len();
    let s = m.check_for_updates().await;
    assert!(!s.supported);
    assert_eq!(s.phase, "idle");
    assert!(m.download_update().await.is_err());
    assert_eq!(rel.server.state.request_heads.lock().unwrap().len(), hits, "no request at all");
    m.shutdown().await;
}

/// A copy watching the newest release (no trusted key built in, or no signed manifest on the release).
async fn watcher(rel: &Release, dir: &Path, keys: TrustedKeys) -> DownloadManager {
    let mut cfg = ManagerConfig::new(dir.join("state.db"), dir.join("dl"));
    cfg.update = Some(UpdateConfig {
        keys,
        policy: Policy { product: "VeloxFetch".into(), url_prefix: rel.prefix(), allow_loopback_http: true },
        feed: rel.server.url("/feed/latest.json"),
        release_page: rel.server.url("/page"),
        latest_release: Some(rel.server.url("/redirext/latest")),
        current: Version(2, 0, 0),
        kind: Some(kind().0.into()),
        allow_loopback_http: true,
        first_check: Duration::from_secs(3600),
        every: Duration::from_secs(3600),
        dir: Some(dir.join("updates")),
    });
    DownloadManager::new(cfg).await.unwrap()
}

/// The newest GitHub release is announced (version, release page) when there is no signed manifest: never
/// downloaded or installed that way; only a tag of the release page counts.
#[tokio::test]
async fn the_newest_release_is_announced_without_a_signed_manifest() {
    let rel = Release::new().await;
    let tag = |v: &str| {
        rel.server.put(&format!("/page/tag/v{v}"), "the release page");
        *rel.server.state.redirect_to.lock().unwrap() = rel.server.url(&format!("/page/tag/v{v}"));
    };
    // No trusted key in this build: the watch alone.
    let dir = temp("watch");
    let m = watcher(&rel, &dir, TrustedKeys::parse("").unwrap()).await;
    assert!(m.update_status().supported, "watched: Settings > Updates checks");
    tag("2.0.1");
    let s = m.check_for_updates().await;
    assert_eq!((s.phase, s.version.as_deref(), s.size), ("elsewhere", Some("2.0.1"), None), "{s:?}");
    assert_eq!(s.release_page.as_deref(), Some(rel.server.url("/page").as_str()));
    assert!(m.download_update().await.is_err(), "nothing is downloaded from a watched release");
    assert!(!dir.join("updates").exists() || std::fs::read_dir(dir.join("updates")).unwrap().count() == 0);
    // The same version, or an older one: up to date.
    tag("2.0.0");
    assert_eq!(m.check_for_updates().await.phase, "upToDate");
    tag("1.9.9");
    assert_eq!(m.check_for_updates().await.phase, "upToDate");
    // An address that is not a tag of the release page, or not a version: refused, nothing announced.
    *rel.server.state.redirect_to.lock().unwrap() = rel.server.url("/elsewhere/tag/v9.9.9");
    rel.server.put("/elsewhere/tag/v9.9.9", "x");
    let s = m.check_for_updates().await;
    assert_eq!((s.phase, s.version.as_deref()), ("failed", None), "{s:?}");
    tag("3.0.0-beta");
    assert_eq!(m.check_for_updates().await.phase, "failed");

    // A signed build whose release has no latest.json (published by hand): the newest release still says.
    let dir = temp("watch-signed");
    let keys = TrustedKeys::parse(&format!("{KEY_PREFIX} {}\n", to_hex(signer().verifying_key().as_bytes()))).unwrap();
    let m = watcher(&rel, &dir, keys).await;
    tag("2.1.0");
    let s = m.check_for_updates().await;
    assert_eq!((s.phase, s.version.as_deref()), ("elsewhere", Some("2.1.0")), "{s:?}");
    // With a signed manifest there, it wins: the package is offered as before.
    rel.publish("2.1.0", 2000, &signer());
    let s = m.check_for_updates().await;
    assert_eq!((s.phase, s.version.as_deref()), ("available", Some("2.1.0")), "{s:?}");
}
