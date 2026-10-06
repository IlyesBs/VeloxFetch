//! Secrets and credentials (Phase 11).
//!
//! Rules this crate makes easy to follow:
//! * A secret is never written to disk in the clear. Anything that must survive a restart (a
//!   browser's cookies for a paused download, a download's `Authorization` header) is *sealed* with
//!   a random 256-bit key. That key lives only in the operating system's credential store
//!   (Windows Credential Manager, macOS Keychain, the Secret Service on Linux), never in a file.
//! * If the credential store is unavailable, callers get an error and must keep the secret in
//!   memory only. There is deliberately no "fall back to a key file".
//! * Secrets and links that may carry secrets are printed through [`Redacted`] / [`redact_url`].

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Mutex;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use zeroize::Zeroizing;

#[derive(Debug)]
pub enum SecretError {
    /// The credential store cannot be used here (no Secret Service, locked, denied...).
    Unavailable(String),
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretError::Unavailable(e) => write!(f, "secure storage unavailable: {e}"),
        }
    }
}

impl std::error::Error for SecretError {}

/// Somewhere that can keep a small secret safely.
pub trait KeyStore: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError>;
    fn set(&self, name: &str, secret: &[u8]) -> Result<(), SecretError>;
    fn delete(&self, name: &str) -> Result<(), SecretError>;
}

impl std::fmt::Debug for dyn KeyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeyStore(..)")
    }
}

/// The operating system's credential store.
pub struct OsKeyStore {
    service: String,
}

impl OsKeyStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self { service: service.into() }
    }

    fn entry(&self, name: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, name).map_err(|e| SecretError::Unavailable(e.to_string()))
    }
}

impl KeyStore for OsKeyStore {
    fn get(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
        match self.entry(name)?.get_secret() {
            Ok(v) => Ok(Some(Zeroizing::new(v))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Unavailable(e.to_string())),
        }
    }

    fn set(&self, name: &str, secret: &[u8]) -> Result<(), SecretError> {
        self.entry(name)?.set_secret(secret).map_err(|e| SecretError::Unavailable(e.to_string()))
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        match self.entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::Unavailable(e.to_string())),
        }
    }
}

/// Keeps secrets in this process only (tests, and debug builds that must not touch the real
/// credential store). Nothing is ever written anywhere.
#[derive(Default)]
pub struct MemoryKeyStore(Mutex<HashMap<String, Vec<u8>>>);

impl KeyStore for MemoryKeyStore {
    fn get(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
        Ok(self.0.lock().unwrap().get(name).map(|v| Zeroizing::new(v.clone())))
    }

    fn set(&self, name: &str, secret: &[u8]) -> Result<(), SecretError> {
        self.0.lock().unwrap().insert(name.to_string(), secret.to_vec());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        self.0.lock().unwrap().remove(name);
        Ok(())
    }
}

/// A store that always fails: models a machine without a usable credential store.
pub struct UnavailableKeyStore;

impl KeyStore for UnavailableKeyStore {
    fn get(&self, _: &str) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
        Err(SecretError::Unavailable("no credential store".into()))
    }
    fn set(&self, _: &str, _: &[u8]) -> Result<(), SecretError> {
        Err(SecretError::Unavailable("no credential store".into()))
    }
    fn delete(&self, _: &str) -> Result<(), SecretError> {
        Err(SecretError::Unavailable("no credential store".into()))
    }
}

const PREFIX: &str = "s1:";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// Authenticated encryption (ChaCha20-Poly1305) of small secrets for storage on disk.
///
/// A sealed value is `s1:` + hex(nonce ‖ ciphertext ‖ tag). The nonce is random per value, and a
/// *context* string is authenticated with it, so a value sealed for one column cannot be moved to
/// another and accepted.
pub struct Sealer {
    key: Zeroizing<[u8; KEY_LEN]>,
}

impl Sealer {
    pub fn from_key(key: [u8; KEY_LEN]) -> Self {
        Self { key: Zeroizing::new(key) }
    }

    /// Load the key called `name` from `store`, creating (and saving) a fresh random one the first
    /// time. Fails, rather than inventing a key that could not be found again, when the store
    /// cannot be written.
    pub fn from_store(store: &dyn KeyStore, name: &str) -> Result<Self, SecretError> {
        if let Some(k) = store.get(name)? {
            if let Ok(arr) = <[u8; KEY_LEN]>::try_from(k.as_slice()) {
                return Ok(Self::from_key(arr));
            }
            // A damaged entry is useless (whatever it sealed can never be opened): replace it.
        }
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        random(&mut key[..]);
        store.set(name, &key[..])?;
        // Read it back: some stores accept a write they cannot later return.
        match store.get(name)? {
            Some(k) if k.as_slice() == &key[..] => Ok(Self::from_key(*key)),
            _ => Err(SecretError::Unavailable("the credential store did not keep the key".into())),
        }
    }

    pub fn seal(&self, plaintext: &[u8], context: &str) -> String {
        let mut nonce = [0u8; NONCE_LEN];
        random(&mut nonce);
        let cipher = ChaCha20Poly1305::new_from_slice(&self.key[..]).expect("32-byte key");
        let ct = cipher
            .encrypt(&Nonce::from(nonce), Payload { msg: plaintext, aad: context.as_bytes() })
            .expect("ChaCha20-Poly1305 encryption cannot fail for in-memory input");
        let mut out = String::with_capacity(PREFIX.len() + 2 * (NONCE_LEN + ct.len()));
        out.push_str(PREFIX);
        for b in nonce.iter().chain(ct.iter()) {
            let _ = write!(out, "{b:02x}");
        }
        out
    }

    /// `None` for anything that is not a value sealed by this key for this context (wrong key,
    /// wrong context, truncated, altered, not sealed at all).
    pub fn open(&self, sealed: &str, context: &str) -> Option<Zeroizing<Vec<u8>>> {
        let hex = sealed.strip_prefix(PREFIX)?;
        if hex.len() < 2 * (NONCE_LEN + 16) || !hex.len().is_multiple_of(2) || !hex.is_ascii() {
            return None;
        }
        let bytes: Vec<u8> = (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()).collect::<Option<_>>()?;
        let (nonce, ct) = bytes.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce.try_into().ok()?;
        let cipher = ChaCha20Poly1305::new_from_slice(&self.key[..]).expect("32-byte key");
        cipher.decrypt(&Nonce::from(nonce), Payload { msg: ct, aad: context.as_bytes() }).ok().map(Zeroizing::new)
    }
}

/// Is this text one of our sealed values (whether or not it can be opened)?
pub fn is_sealed(s: &str) -> bool {
    s.starts_with(PREFIX)
}

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sealer(..)")
    }
}

fn random(buf: &mut [u8]) {
    getrandom::getrandom(buf).expect("the operating system random number generator failed");
}

/// Prints as `***` in `Debug` and `Display`, so a secret in a struct that gets logged with `{:?}` stays hidden.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Redacted<T>(pub T);

impl<T> std::fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("***")
    }
}

impl<T> std::fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("***")
    }
}

/// A link made safe to log or show in an error: no user name or password, no query and no
/// fragment (signed links carry their token there). Text that is not a link becomes `<invalid link>`.
pub fn redact_url(s: &str) -> String {
    match url::Url::parse(s.trim()) {
        Ok(mut u) => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            let had_query = u.query().is_some();
            u.set_query(None);
            u.set_fragment(None);
            if had_query {
                format!("{u}?…")
            } else {
                u.to_string()
            }
        }
        Err(_) => "<invalid link>".to_string(),
    }
}

/// Header values that must never be logged.
pub fn is_sensitive_header(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "cookie" | "authorization" | "proxy-authorization" | "set-cookie" | "referer")
}

/// `name: value` pairs for logging, with sensitive values hidden.
pub fn redact_headers(h: &[(String, String)]) -> String {
    h.iter().map(|(k, v)| if is_sensitive_header(k) { format!("{k}: ***") } else { format!("{k}: {v}") }).collect::<Vec<_>>().join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sealer() -> Sealer {
        Sealer::from_key([7u8; 32])
    }

    #[test]
    fn sealed_values_round_trip_and_hide_the_plaintext() {
        let s = sealer();
        let sealed = s.seal(b"sid=SECRETCOOKIE", "ctx");
        assert!(is_sealed(&sealed) && !sealed.contains("SECRETCOOKIE") && !sealed.contains("53454352455443")); // not even as hex
        assert_eq!(s.open(&sealed, "ctx").unwrap().as_slice(), b"sid=SECRETCOOKIE");
        // Random nonce: sealing twice differs.
        assert_ne!(s.seal(b"x", "ctx"), s.seal(b"x", "ctx"));
        // Empty and large values.
        assert_eq!(s.open(&s.seal(b"", "c"), "c").unwrap().len(), 0);
        let big = vec![0xabu8; 100_000];
        assert_eq!(s.open(&s.seal(&big, "c"), "c").unwrap().as_slice(), &big[..]);
    }

    #[test]
    fn tampering_wrong_key_and_wrong_context_are_all_refused() {
        let s = sealer();
        let sealed = s.seal(b"secret", "column-a");
        assert!(s.open(&sealed, "column-b").is_none(), "another column's context");
        assert!(Sealer::from_key([8u8; 32]).open(&sealed, "column-a").is_none(), "another key");
        let mut flipped = sealed.clone().into_bytes();
        let last = flipped.len() - 1;
        flipped[last] = if flipped[last] == b'0' { b'1' } else { b'0' };
        assert!(s.open(std::str::from_utf8(&flipped).unwrap(), "column-a").is_none(), "altered tag");
        for bad in
            ["", "s1:", "s1:zz", "s1:00", "plain text", &sealed[..sealed.len() - 2], &sealed[..20], "s1:\u{e9}\u{e9}\u{e9}\u{e9}", "S1:00"]
        {
            assert!(s.open(bad, "column-a").is_none(), "{bad:?}");
        }
        assert!(!is_sealed("{\"cookie\":\"x\"}"));
    }

    #[test]
    fn the_key_is_created_once_then_reused_and_a_broken_store_is_an_error() {
        let store = MemoryKeyStore::default();
        let a = Sealer::from_store(&store, "k").unwrap();
        let b = Sealer::from_store(&store, "k").unwrap();
        assert_eq!(b.open(&a.seal(b"x", "c"), "c").unwrap().as_slice(), b"x", "same key the second time");
        let other = Sealer::from_store(&store, "other").unwrap();
        assert!(other.open(&a.seal(b"x", "c"), "c").is_none(), "names are separate keys");
        assert!(Sealer::from_store(&UnavailableKeyStore, "k").is_err(), "no store: an error, never a key kept in a file");
        // A damaged entry is replaced instead of blocking forever.
        store.set("bad", b"short").unwrap();
        assert!(Sealer::from_store(&store, "bad").is_ok());
        assert_eq!(store.get("bad").unwrap().unwrap().len(), 32);
        // A store that swallows writes is detected.
        struct Forgetful;
        impl KeyStore for Forgetful {
            fn get(&self, _: &str) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
                Ok(None)
            }
            fn set(&self, _: &str, _: &[u8]) -> Result<(), SecretError> {
                Ok(())
            }
            fn delete(&self, _: &str) -> Result<(), SecretError> {
                Ok(())
            }
        }
        assert!(Sealer::from_store(&Forgetful, "k").is_err());
    }

    #[test]
    fn nothing_secret_is_printed() {
        assert_eq!(format!("{:?}", sealer()), "Sealer(..)");
        assert_eq!(format!("{:?} {}", Redacted("pw"), Redacted("pw")), "*** ***");
        assert_eq!(redact_url("https://user:pw@h.test/a/b.zip?token=abc#frag"), "https://h.test/a/b.zip?…");
        assert_eq!(redact_url("https://h.test/a.zip"), "https://h.test/a.zip");
        assert_eq!(redact_url("not a link"), "<invalid link>");
        let h = vec![
            ("cookie".to_string(), "sid=1".to_string()),
            ("Authorization".into(), "Basic xx".into()),
            ("user-agent".into(), "UA".into()),
        ];
        let r = redact_headers(&h);
        assert!(!r.contains("sid=1") && !r.contains("Basic") && r.contains("UA"));
    }

    /// Talks to the real credential store of the machine. Off by default so that running the tests
    /// never touches anybody's credentials; run with `VELOX_TEST_OS_KEYRING=1`.
    #[test]
    fn the_real_os_store_round_trips() {
        if std::env::var_os("VELOX_TEST_OS_KEYRING").is_none() {
            return;
        }
        let store = OsKeyStore::new(format!("app.velox.fetch.test-{}", std::process::id()));
        assert!(store.get("k").unwrap().is_none());
        let a = Sealer::from_store(&store, "k").unwrap();
        let b = Sealer::from_store(&store, "k").unwrap();
        assert_eq!(b.open(&a.seal(b"round trip", "c"), "c").unwrap().as_slice(), b"round trip");
        store.delete("k").unwrap();
        assert!(store.get("k").unwrap().is_none());
        store.delete("k").unwrap(); // deleting twice is fine
    }
}
