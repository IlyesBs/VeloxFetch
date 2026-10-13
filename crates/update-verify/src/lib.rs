//! Checks a VeloxFetch update before anything is written or run (Phase 19).
//!
//! ```text
//! latest.json + latest.json.sig  --Ed25519, a key compiled into the app-->  Manifest
//!     --> same product, newer version, not older than one already seen  --> the package for this system
//!     --> its file: exact size and SHA-256 from the signed manifest  --> only then installed
//! ```
//! The private key never leaves the owner's computer (`scripts/update-key.mjs`, passphrase-encrypted). The
//! app trusts only the public keys in `apps/desktop/src-tauri/update-keys.txt`. A key is replaced by
//! shipping a version that trusts the old and the new key, then signing with the new one
//! (`docs/auto-update.md`).
//!
//! This crate does no network, writes nothing and runs nothing: the callers fetch, store and install.

use std::cmp::Ordering;
use std::io::Read;
use std::path::Path;

use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Signed bytes start with this, so a signature over anything else (another file, another format) never
/// passes as a manifest signature.
pub const SIGNING_CONTEXT: &[u8] = b"VeloxFetch update manifest v1\n";
/// A line of the trusted keys file.
pub const KEY_PREFIX: &str = "velox-update-key-v1";
/// The signature file's only line starts with this.
pub const SIG_PREFIX: &str = "velox-update-sig-v1";
/// The manifest is small; anything bigger is refused before it is parsed.
pub const MAX_MANIFEST: usize = 64 * 1024;
pub const MAX_SIGNATURE: usize = 1024;
/// No package is ever bigger than this.
pub const MAX_PACKAGE: u64 = 2 * 1024 * 1024 * 1024;
const SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    #[error("the update information is too large")]
    TooLarge,
    #[error("the update signature is not readable")]
    BadSignatureFormat,
    #[error("the update is signed with a key this version does not trust")]
    UnknownKey,
    #[error("the update signature does not match")]
    BadSignature,
    #[error("the update information is not readable: {0}")]
    Malformed(String),
    #[error("the update is for another program")]
    WrongProduct,
    #[error("the update lists a package that is not allowed: {0}")]
    BadPackage(String),
    #[error("the update information is older than one already seen")]
    Stale,
    #[error("the downloaded update does not have the expected size")]
    SizeMismatch,
    #[error("the downloaded update does not match its signed checksum")]
    HashMismatch,
    #[error("the update file could not be read: {0}")]
    Io(String),
    #[error("no trusted update key is set in this build")]
    NoKeys,
}

impl UpdateError {
    /// A short stable code for the UI's texts (never a message from the network).
    pub fn code(&self) -> &'static str {
        match self {
            UpdateError::TooLarge | UpdateError::Malformed(_) | UpdateError::BadSignatureFormat => "malformed",
            UpdateError::UnknownKey | UpdateError::BadSignature => "signature",
            UpdateError::WrongProduct | UpdateError::BadPackage(_) => "rejected",
            UpdateError::Stale => "stale",
            UpdateError::SizeMismatch | UpdateError::HashMismatch => "corrupt",
            UpdateError::Io(_) => "disk",
            UpdateError::NoKeys => "off",
        }
    }
}

/// The public keys the app trusts, compiled in.
#[derive(Debug, Clone, Default)]
pub struct TrustedKeys {
    keys: Vec<([u8; 8], VerifyingKey)>,
}

/// The key id: the first 8 bytes of the public key's SHA-256, in hex.
pub fn key_id(public: &[u8; 32]) -> [u8; 8] {
    let h = Sha256::digest(public);
    let mut id = [0u8; 8];
    id.copy_from_slice(&h[..8]);
    id
}

impl TrustedKeys {
    /// Lines `velox-update-key-v1 <64 hex>`; `#` comments and blank lines are ignored. Any other line is an
    /// error (a typo must not silently drop a key). A weak key (small order) is refused.
    pub fn parse(text: &str) -> Result<Self, UpdateError> {
        let mut keys = Vec::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let mut parts = line.split_whitespace();
            let (Some(KEY_PREFIX), Some(hex), None) = (parts.next(), parts.next(), parts.next()) else {
                return Err(UpdateError::Malformed("a line of the trusted keys is not a key".into()));
            };
            let bytes: [u8; 32] = from_hex(hex).ok_or_else(|| UpdateError::Malformed("a trusted key is not 32 bytes of hex".into()))?;
            let key = VerifyingKey::from_bytes(&bytes).map_err(|_| UpdateError::Malformed("a trusted key is not a valid key".into()))?;
            if key.is_weak() {
                return Err(UpdateError::Malformed("a trusted key is weak".into()));
            }
            let id = key_id(&bytes);
            if !keys.iter().any(|(k, _)| *k == id) {
                keys.push((id, key));
            }
        }
        Ok(Self { keys })
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }
}

/// What the manifest must name, and where its packages may come from.
#[derive(Debug, Clone)]
pub struct Policy {
    pub product: String,
    /// Every package address starts with this (`https://github.com/<owner>/<repo>/releases/download/`).
    pub url_prefix: String,
    /// Tests only: a prefix on `http://127.0.0.1:` is accepted. Always false in the app.
    pub allow_loopback_http: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    schema: u32,
    product: String,
    version: String,
    released: u64,
    packages: Vec<RawPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPackage {
    target: String,
    name: String,
    url: String,
    size: u64,
    sha256: String,
}

/// A checked manifest: signed by a trusted key, for this product, every package well formed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version: Version,
    /// When it was signed (seconds since 1970). Never goes back between two accepted manifests.
    pub released: u64,
    pub packages: Vec<Package>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// `<os>-<arch>-<kind>`, e.g. `windows-x86_64-nsis`, `linux-x86_64-appimage`, `macos-aarch64-dmg`.
    pub target: String,
    /// The file's name (letters, digits, `.`, `_`, `-`, `+`), with the kind's extension.
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: [u8; 32],
}

/// `major.minor.patch`, numbers only (no pre-release: the update channel carries stable versions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    pub fn parse(s: &str) -> Option<Self> {
        let mut it = s.split('.');
        let mut num = || -> Option<u32> {
            let p = it.next()?;
            if p.is_empty() || p.len() > 6 || !p.bytes().all(|b| b.is_ascii_digit()) || (p.len() > 1 && p.starts_with('0')) {
                return None;
            }
            p.parse().ok()
        };
        let v = Version(num()?, num()?, num()?);
        it.next().is_none().then_some(v)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

fn from_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, b) in out.iter_mut().enumerate() {
        let pair = s.get(i * 2..i * 2 + 2)?;
        // Lower-case only: one spelling per value.
        if !pair.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
            return None;
        }
        *b = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The extension each kind of package must have.
fn kind_extension(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "nsis" => ".exe",
        "appimage" => ".AppImage",
        "deb" => ".deb",
        "rpm" => ".rpm",
        "dmg" => ".dmg",
        _ => return None,
    })
}

fn valid_target(t: &str) -> bool {
    let mut parts = t.split('-');
    let (Some(os), Some(arch), Some(kind), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let os_ok = matches!((os, kind), ("windows", "nsis") | ("linux", "appimage" | "deb" | "rpm") | ("macos", "dmg"));
    os_ok && matches!(arch, "x86_64" | "aarch64")
}

fn check_package(p: RawPackage, policy: &Policy) -> Result<Package, UpdateError> {
    let bad = |why: &str| UpdateError::BadPackage(format!("{}: {why}", p.target));
    if !valid_target(&p.target) {
        return Err(bad("unknown system"));
    }
    let ext = p.target.rsplit('-').next().and_then(kind_extension).ok_or_else(|| bad("unknown kind"))?;
    let name_ok = !p.name.is_empty()
        && p.name.len() <= 128
        && !p.name.starts_with('.')
        && p.name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
        && !p.name.contains("..")
        && p.name.ends_with(ext);
    if !name_ok {
        return Err(bad("bad file name"));
    }
    // The address is the release's own folder plus the file's name: nothing else (no query, no other host).
    let rest = p.url.strip_prefix(&policy.url_prefix).ok_or_else(|| bad("address outside the release folder"))?;
    let rest_ok = rest.split_once('/').is_some_and(|(tag, file)| {
        tag.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
            && !tag.contains("..")
            && tag.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            && file == p.name
    });
    let secure =
        policy.url_prefix.starts_with("https://") || (policy.allow_loopback_http && policy.url_prefix.starts_with("http://127.0.0.1:"));
    if !rest_ok || !secure {
        return Err(bad("bad address"));
    }
    if p.size == 0 || p.size > MAX_PACKAGE {
        return Err(bad("bad size"));
    }
    let sha256 = from_hex::<32>(&p.sha256).ok_or_else(|| bad("bad checksum"))?;
    Ok(Package { target: p.target, name: p.name, url: p.url, size: p.size, sha256 })
}

/// Check `manifest` against `signature` and the trusted keys, then its content. Nothing of the manifest is
/// read before its signature has passed.
pub fn verify_manifest(manifest: &[u8], signature: &[u8], keys: &TrustedKeys, policy: &Policy) -> Result<Manifest, UpdateError> {
    if keys.is_empty() {
        return Err(UpdateError::NoKeys);
    }
    if manifest.len() > MAX_MANIFEST || signature.len() > MAX_SIGNATURE {
        return Err(UpdateError::TooLarge);
    }
    let sig_text = std::str::from_utf8(signature).map_err(|_| UpdateError::BadSignatureFormat)?.trim();
    let mut parts = sig_text.split_whitespace();
    let (Some(SIG_PREFIX), Some(id_hex), Some(sig_hex), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(UpdateError::BadSignatureFormat);
    };
    let id: [u8; 8] = from_hex(id_hex).ok_or(UpdateError::BadSignatureFormat)?;
    let sig: [u8; 64] = from_hex(sig_hex).ok_or(UpdateError::BadSignatureFormat)?;
    let key = keys.keys.iter().find(|(k, _)| *k == id).map(|(_, k)| k).ok_or(UpdateError::UnknownKey)?;
    let mut message = Vec::with_capacity(SIGNING_CONTEXT.len() + manifest.len());
    message.extend_from_slice(SIGNING_CONTEXT);
    message.extend_from_slice(manifest);
    // Strict: no malleable or small-order signatures.
    key.verify_strict(&message, &Signature::from_bytes(&sig)).map_err(|_| UpdateError::BadSignature)?;

    let raw: RawManifest = serde_json::from_slice(manifest).map_err(|e| UpdateError::Malformed(e.to_string()))?;
    if raw.schema != SCHEMA {
        return Err(UpdateError::Malformed("unknown schema".into()));
    }
    if raw.product != policy.product {
        return Err(UpdateError::WrongProduct);
    }
    let version = Version::parse(&raw.version).ok_or_else(|| UpdateError::Malformed("bad version".into()))?;
    if raw.packages.is_empty() || raw.packages.len() > 32 {
        return Err(UpdateError::Malformed("bad package list".into()));
    }
    let mut packages: Vec<Package> = Vec::new();
    for p in raw.packages {
        let p = check_package(p, policy)?;
        if packages.iter().any(|q| q.target == p.target) {
            return Err(UpdateError::BadPackage(format!("{}: listed twice", p.target)));
        }
        packages.push(p);
    }
    Ok(Manifest { version, released: raw.released, packages })
}

/// What a checked manifest means for this copy of the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    UpToDate,
    /// A newer version, with the package for this system.
    Available(Package),
    /// A newer version exists, but not for this system (or this kind of installation).
    NoPackage,
}

/// `last_released`: the newest `released` of a manifest accepted before. An older manifest (an attacker
/// replaying an old, genuinely signed one to hide a fix) is refused, never taken as "up to date".
pub fn decide(current: Version, manifest: &Manifest, last_released: Option<u64>, target: Option<&str>) -> Result<Decision, UpdateError> {
    if last_released.is_some_and(|last| manifest.released < last) {
        return Err(UpdateError::Stale);
    }
    if manifest.version.cmp(&current) != Ordering::Greater {
        return Ok(Decision::UpToDate);
    }
    Ok(match target.and_then(|t| manifest.packages.iter().find(|p| p.target == t)) {
        Some(p) => Decision::Available(p.clone()),
        None => Decision::NoPackage,
    })
}

/// Hash a reader that must hold exactly `size` bytes (one more is never read past).
pub fn check_reader(mut r: impl Read, size: u64, sha256: &[u8; 32]) -> Result<(), UpdateError> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = r.read(&mut buf).map_err(|e| UpdateError::Io(e.to_string()))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > size {
            return Err(UpdateError::SizeMismatch);
        }
        h.update(&buf[..n]);
    }
    if total != size {
        return Err(UpdateError::SizeMismatch);
    }
    let got: [u8; 32] = h.finalize().into();
    // Not secret, but compare the whole value anyway.
    if got.iter().zip(sha256).fold(0u8, |acc, (a, b)| acc | (a ^ b)) != 0 {
        return Err(UpdateError::HashMismatch);
    }
    Ok(())
}

/// Check the file at `path` against a package from a verified manifest.
pub fn check_file(path: &Path, package: &Package) -> Result<(), UpdateError> {
    let f = std::fs::File::open(path).map_err(|e| UpdateError::Io(e.to_string()))?;
    check_reader(f, package.size, &package.sha256)
}

/// This system's package target for an installation kind (`nsis`, `appimage`, `deb`, `rpm`, `dmg`).
pub fn target_for(kind: &str) -> Option<String> {
    let os = match std::env::consts::OS {
        "windows" => "windows",
        "linux" => "linux",
        "macos" => "macos",
        _ => return None,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        _ => return None,
    };
    let t = format!("{os}-{arch}-{kind}");
    valid_target(&t).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const PREFIX: &str = "https://github.com/o/r/releases/download/";

    fn policy() -> Policy {
        Policy { product: "VeloxFetch".into(), url_prefix: PREFIX.into(), allow_loopback_http: false }
    }

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn keys_line(k: &SigningKey) -> String {
        format!("{KEY_PREFIX} {}\n", to_hex(k.verifying_key().as_bytes()))
    }

    fn sign(k: &SigningKey, manifest: &[u8]) -> Vec<u8> {
        let mut m = SIGNING_CONTEXT.to_vec();
        m.extend_from_slice(manifest);
        let sig = k.sign(&m);
        format!("{SIG_PREFIX} {} {}\n", to_hex(&key_id(k.verifying_key().as_bytes())), to_hex(&sig.to_bytes())).into_bytes()
    }

    fn manifest(version: &str, released: u64) -> Vec<u8> {
        serde_json::json!({
            "schema": 1, "product": "VeloxFetch", "version": version, "released": released,
            "packages": [
                {"target": "linux-x86_64-appimage", "name": "VeloxFetch_2.0.0_amd64.AppImage",
                 "url": format!("{PREFIX}v2.0.0/VeloxFetch_2.0.0_amd64.AppImage"), "size": 5, "sha256": to_hex(&Sha256::digest(b"hello"))},
                {"target": "windows-x86_64-nsis", "name": "VeloxFetch_2.0.0_x64-setup.exe",
                 "url": format!("{PREFIX}v2.0.0/VeloxFetch_2.0.0_x64-setup.exe"), "size": 5, "sha256": to_hex(&Sha256::digest(b"hello"))}
            ]
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn a_manifest_signed_by_a_trusted_key_is_accepted() {
        let k = key(1);
        let keys = TrustedKeys::parse(&format!("# owner key\n\n{}", keys_line(&k))).unwrap();
        let m = manifest("2.0.0", 100);
        let got = verify_manifest(&m, &sign(&k, &m), &keys, &policy()).unwrap();
        assert_eq!(got.version, Version(2, 0, 0));
        assert_eq!(got.packages.len(), 2);
    }

    #[test]
    fn any_change_to_the_manifest_or_signature_is_refused() {
        let k = key(1);
        let keys = TrustedKeys::parse(&keys_line(&k)).unwrap();
        let m = manifest("2.0.0", 100);
        let sig = sign(&k, &m);
        // One byte changed anywhere in the manifest.
        for i in [0, m.len() / 2, m.len() - 1] {
            let mut t = m.clone();
            t[i] ^= 1;
            assert!(verify_manifest(&t, &sig, &keys, &policy()).is_err(), "byte {i}");
        }
        // A signature over the bare manifest (no context) does not pass.
        let bare = k.sign(&m);
        let bare = format!("{SIG_PREFIX} {} {}", to_hex(&key_id(k.verifying_key().as_bytes())), to_hex(&bare.to_bytes()));
        assert_eq!(verify_manifest(&m, bare.as_bytes(), &keys, &policy()), Err(UpdateError::BadSignature));
        // Another key, even when it claims the trusted key's id.
        let other = key(2);
        let mut forged = sign(&other, &m);
        let trusted_id = to_hex(&key_id(k.verifying_key().as_bytes()));
        let other_id = to_hex(&key_id(other.verifying_key().as_bytes()));
        assert_eq!(verify_manifest(&m, &forged, &keys, &policy()), Err(UpdateError::UnknownKey));
        forged = String::from_utf8(forged).unwrap().replace(&other_id, &trusted_id).into_bytes();
        assert_eq!(verify_manifest(&m, &forged, &keys, &policy()), Err(UpdateError::BadSignature));
        // Malformed signature files.
        for s in [
            "",
            "velox-update-sig-v1",
            "velox-update-sig-v2 00 00",
            "velox-update-sig-v1 zz zz",
            &format!("{} extra", String::from_utf8_lossy(&sig)),
        ] {
            assert!(verify_manifest(&m, s.as_bytes(), &keys, &policy()).is_err(), "{s}");
        }
        // Upper-case hex is a second spelling of the same signature: refused.
        let upper = String::from_utf8(sig.clone()).unwrap().to_uppercase().replace("VELOX-UPDATE-SIG-V1", SIG_PREFIX);
        assert_eq!(verify_manifest(&m, upper.as_bytes(), &keys, &policy()), Err(UpdateError::BadSignatureFormat));
    }

    #[test]
    fn without_trusted_keys_nothing_is_accepted() {
        let k = key(1);
        let m = manifest("2.0.0", 100);
        assert_eq!(verify_manifest(&m, &sign(&k, &m), &TrustedKeys::default(), &policy()), Err(UpdateError::NoKeys));
        assert!(TrustedKeys::parse("# no keys yet\n").unwrap().is_empty());
        assert!(TrustedKeys::parse("velox-update-key-v1 1234").is_err());
        assert!(TrustedKeys::parse("ssh-ed25519 AAAA").is_err());
        // The identity point is a weak key.
        let mut weak = [0u8; 32];
        weak[0] = 1;
        assert!(TrustedKeys::parse(&format!("{KEY_PREFIX} {}", to_hex(&weak))).is_err());
    }

    #[test]
    fn a_rotated_key_set_accepts_both_keys() {
        let (old, new) = (key(1), key(3));
        let keys = TrustedKeys::parse(&format!("{}{}", keys_line(&old), keys_line(&new))).unwrap();
        assert_eq!(keys.len(), 2);
        let m = manifest("2.0.0", 100);
        assert!(verify_manifest(&m, &sign(&old, &m), &keys, &policy()).is_ok());
        assert!(verify_manifest(&m, &sign(&new, &m), &keys, &policy()).is_ok());
    }

    #[test]
    fn signed_content_is_still_checked() {
        let k = key(1);
        let keys = TrustedKeys::parse(&keys_line(&k)).unwrap();
        let check = |v: serde_json::Value| {
            let m = v.to_string().into_bytes();
            verify_manifest(&m, &sign(&k, &m), &keys, &policy())
        };
        let pkg = |target: &str, name: &str, url: &str| serde_json::json!({"target": target, "name": name, "url": url, "size": 5, "sha256": to_hex(&[7u8; 32])});
        let doc = |packages: serde_json::Value| serde_json::json!({"schema": 1, "product": "VeloxFetch", "version": "2.0.0", "released": 1, "packages": packages});
        let ok = pkg("linux-x86_64-deb", "a.deb", &format!("{PREFIX}v2.0.0/a.deb"));
        assert!(check(doc(serde_json::json!([ok.clone()]))).is_ok());
        let mut wrong_product = doc(serde_json::json!([ok.clone()]));
        wrong_product["product"] = "Other".into();
        assert_eq!(check(wrong_product), Err(UpdateError::WrongProduct));
        let mut extra_field = doc(serde_json::json!([ok.clone()]));
        extra_field["run"] = "x".into();
        assert!(matches!(check(extra_field), Err(UpdateError::Malformed(_))));
        for bad in [
            pkg("linux-x86_64-deb", "a.deb", "http://github.com/o/r/releases/download/v2.0.0/a.deb"),
            pkg("linux-x86_64-deb", "a.deb", "https://evil.example/o/r/releases/download/v2.0.0/a.deb"),
            pkg("linux-x86_64-deb", "a.deb", &format!("{PREFIX}v2.0.0/b.deb")),
            pkg("linux-x86_64-deb", "a.deb", &format!("{PREFIX}v2.0.0/a.deb?x=1")),
            pkg("linux-x86_64-deb", "a.deb", &format!("{PREFIX}../a.deb")),
            pkg("linux-x86_64-deb", "a.exe", &format!("{PREFIX}v2.0.0/a.exe")),
            pkg("linux-x86_64-deb", "../a.deb", &format!("{PREFIX}v2.0.0/../a.deb")),
            pkg("windows-x86_64-deb", "a.deb", &format!("{PREFIX}v2.0.0/a.deb")),
            pkg("linux-riscv-deb", "a.deb", &format!("{PREFIX}v2.0.0/a.deb")),
        ] {
            assert!(matches!(check(doc(serde_json::json!([bad.clone()]))), Err(UpdateError::BadPackage(_))), "{bad}");
        }
        assert!(matches!(check(doc(serde_json::json!([ok.clone(), ok.clone()]))), Err(UpdateError::BadPackage(_))));
        assert!(matches!(check(doc(serde_json::json!([]))), Err(UpdateError::Malformed(_))));
        let mut zero = ok.clone();
        zero["size"] = 0.into();
        assert!(check(doc(serde_json::json!([zero]))).is_err());
        let mut upper = ok;
        upper["sha256"] = to_hex(&[0xab; 32]).to_uppercase().into();
        assert!(check(doc(serde_json::json!([upper]))).is_err());
    }

    #[test]
    fn plain_http_is_only_for_local_tests() {
        let k = key(1);
        let keys = TrustedKeys::parse(&keys_line(&k)).unwrap();
        let local = "http://127.0.0.1:9/r/";
        let m = serde_json::json!({"schema": 1, "product": "VeloxFetch", "version": "2.0.0", "released": 1, "packages": [
            {"target": "linux-x86_64-deb", "name": "a.deb", "url": format!("{local}v2/a.deb"), "size": 5, "sha256": to_hex(&[7u8; 32])}]})
        .to_string()
        .into_bytes();
        let mut p = Policy { product: "VeloxFetch".into(), url_prefix: local.into(), allow_loopback_http: false };
        assert!(matches!(verify_manifest(&m, &sign(&k, &m), &keys, &p), Err(UpdateError::BadPackage(_))));
        p.allow_loopback_http = true;
        assert!(verify_manifest(&m, &sign(&k, &m), &keys, &p).is_ok());
        p.url_prefix = "http://example.com/r/".into();
        assert!(verify_manifest(&m, &sign(&k, &m), &keys, &p).is_err());
    }

    #[test]
    fn versions_are_strict_and_never_go_back() {
        assert_eq!(Version::parse("1.0.0"), Some(Version(1, 0, 0)));
        assert_eq!(Version::parse("10.20.300"), Some(Version(10, 20, 300)));
        for bad in ["1.0", "1.0.0.0", "01.0.0", "1.0.0-rc1", "v1.0.0", "1..0", " 1.0.0", "1.0.x", "9999999.0.0"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert!(Version(1, 10, 0) > Version(1, 9, 9));
        let k = key(1);
        let keys = TrustedKeys::parse(&keys_line(&k)).unwrap();
        let m = manifest("2.0.0", 100);
        let m = verify_manifest(&m, &sign(&k, &m), &keys, &policy()).unwrap();
        let t = Some("linux-x86_64-appimage");
        assert!(matches!(decide(Version(1, 0, 0), &m, None, t), Ok(Decision::Available(_))));
        assert_eq!(decide(Version(2, 0, 0), &m, None, t), Ok(Decision::UpToDate));
        // A newer installed version is never "updated" to an older one.
        assert_eq!(decide(Version(3, 0, 0), &m, None, t), Ok(Decision::UpToDate));
        // An older manifest than one already seen (a replay) is refused.
        assert_eq!(decide(Version(1, 0, 0), &m, Some(101), t), Err(UpdateError::Stale));
        assert!(decide(Version(1, 0, 0), &m, Some(100), t).is_ok());
        assert_eq!(decide(Version(1, 0, 0), &m, None, Some("macos-aarch64-dmg")), Ok(Decision::NoPackage));
        assert_eq!(decide(Version(1, 0, 0), &m, None, None), Ok(Decision::NoPackage));
    }

    #[test]
    fn a_package_file_must_have_the_exact_size_and_hash() {
        let sha: [u8; 32] = Sha256::digest(b"hello").into();
        assert!(check_reader(&b"hello"[..], 5, &sha).is_ok());
        assert_eq!(check_reader(&b"hell"[..], 5, &sha), Err(UpdateError::SizeMismatch));
        assert_eq!(check_reader(&b"hello!"[..], 5, &sha), Err(UpdateError::SizeMismatch));
        assert_eq!(check_reader(&b"jello"[..], 5, &sha), Err(UpdateError::HashMismatch));
        assert!(check_file(
            Path::new("/definitely/not/here"),
            &Package { target: "linux-x86_64-deb".into(), name: "a.deb".into(), url: String::new(), size: 5, sha256: sha }
        )
        .is_err());
    }

    #[test]
    fn too_large_inputs_are_refused_before_parsing() {
        let k = key(1);
        let keys = TrustedKeys::parse(&keys_line(&k)).unwrap();
        let big = vec![b' '; MAX_MANIFEST + 1];
        assert_eq!(verify_manifest(&big, &sign(&k, &big), &keys, &policy()), Err(UpdateError::TooLarge));
    }

    #[test]
    fn targets_name_the_system_and_kind() {
        assert!(valid_target("windows-x86_64-nsis"));
        assert!(valid_target("macos-aarch64-dmg"));
        assert!(!valid_target("macos-aarch64-deb"));
        assert!(!valid_target("linux-x86_64-appimage-x"));
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            assert_eq!(target_for("deb").as_deref(), Some("linux-x86_64-deb"));
            assert_eq!(target_for("nsis"), None);
        }
    }
}
