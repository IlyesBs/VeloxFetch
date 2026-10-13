//! The manifest and signature in `fixtures/` were made by `scripts/update-key.mjs` (Node's Ed25519) with a
//! throwaway key whose private half was deleted: what the release tool signs, the app accepts, and nothing else.

use update_verify::{check_reader, decide, verify_manifest, Decision, Policy, TrustedKeys, UpdateError, Version};

const MANIFEST: &[u8] = include_bytes!("fixtures/latest.json");
const SIGNATURE: &[u8] = include_bytes!("fixtures/latest.json.sig");
const KEYS: &str = include_str!("fixtures/keys.txt");

fn policy() -> Policy {
    Policy {
        product: "VeloxFetch".into(),
        url_prefix: "https://github.com/IlyesBs/VeloxFetch/releases/download/".into(),
        allow_loopback_http: false,
    }
}

#[test]
fn a_manifest_signed_by_the_release_tool_is_accepted() {
    let keys = TrustedKeys::parse(KEYS).unwrap();
    let m = verify_manifest(MANIFEST, SIGNATURE, &keys, &policy()).unwrap();
    assert_eq!(m.version, Version(2, 0, 0));
    let Ok(Decision::Available(p)) = decide(Version(1, 0, 0), &m, None, Some("linux-x86_64-appimage")) else { panic!("no package") };
    assert!(check_reader(&b"hello"[..], p.size, &p.sha256).is_ok());
    let Ok(Decision::Available(p)) = decide(Version(1, 0, 0), &m, None, Some("windows-x86_64-nsis")) else { panic!("no package") };
    assert!(check_reader(&b"world!"[..], p.size, &p.sha256).is_ok());
    assert_eq!(check_reader(&b"world?"[..], p.size, &p.sha256), Err(UpdateError::HashMismatch));
}

#[test]
fn the_release_tool_signature_does_not_cover_a_changed_manifest() {
    let keys = TrustedKeys::parse(KEYS).unwrap();
    let changed = String::from_utf8(MANIFEST.to_vec()).unwrap().replace("\"size\": 5", "\"size\": 50");
    assert_eq!(verify_manifest(changed.as_bytes(), SIGNATURE, &keys, &policy()), Err(UpdateError::BadSignature));
    // With CRLF line ends (a checkout that converted the file), the bytes differ: refused, not reinterpreted.
    let crlf = String::from_utf8(MANIFEST.to_vec()).unwrap().replace('\n', "\r\n");
    assert_eq!(verify_manifest(crlf.as_bytes(), SIGNATURE, &keys, &policy()), Err(UpdateError::BadSignature));
}

#[test]
fn the_app_does_not_trust_the_throwaway_test_key() {
    let shipped = include_str!("../../../apps/desktop/src-tauri/update-keys.txt");
    let test_key = KEYS.lines().find(|l| l.starts_with("velox-update-key-v1")).unwrap();
    assert!(!shipped.contains(test_key.split_whitespace().nth(1).unwrap()));
}
