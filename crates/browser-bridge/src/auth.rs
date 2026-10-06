//! Mutual authentication of the host <-> app link.
//!
//! The app writes a random 32-byte token into its data directory when it starts. A process that
//! can read that file (i.e. the same user) can prove knowledge of it. Both sides prove it with
//! HMAC-SHA256 over fresh nonces, and **the token itself never crosses the link**, so a process
//! that squatted on the pipe name learns nothing, and cannot impersonate the app either.

use std::fmt::Write as _;
use std::path::Path;

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub const TOKEN_FILE: &str = "bridge.token";
pub const NONCE_LEN: usize = 16;

#[derive(Clone, PartialEq, Eq)]
pub struct Token([u8; 32]);

// Never print the token.
impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(..)")
    }
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("the operating system random number generator failed");
    b
}

pub fn to_hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()).collect()
}

impl Token {
    pub fn generate() -> Self {
        Self(random_bytes::<32>())
    }

    /// Create the token file (replacing any old one), readable only by the current user where the
    /// platform lets us say so (on Windows the per-user profile directory already is).
    pub fn create_file(dir: &Path) -> std::io::Result<Self> {
        let t = Self::generate();
        t.write_file(dir)?;
        Ok(t)
    }

    /// Write this token into `dir` too (the folder of a browser installed in a sandbox), same protection.
    pub fn write_file(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let t = self;
        let path = dir.join(TOKEN_FILE);
        let _ = std::fs::remove_file(&path);
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
            f.write_all(to_hex(&t.0).as_bytes())?;
        }
        #[cfg(not(unix))]
        {
            use std::io::Write;
            // Exclusive create (the old file was just removed): never written through a link that
            // someone planted at this name.
            std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?.write_all(to_hex(&t.0).as_bytes())?;
        }
        Ok(())
    }

    pub fn read_file(dir: &Path) -> std::io::Result<Self> {
        let s = std::fs::read_to_string(dir.join(TOKEN_FILE))?;
        let bytes = from_hex(s.trim()).filter(|b| b.len() == 32).ok_or_else(|| std::io::Error::other("token file is corrupt"))?;
        let mut a = [0u8; 32];
        a.copy_from_slice(&bytes);
        Ok(Self(a))
    }

    fn mac(&self, role: &str, server_nonce: &[u8], client_nonce: &[u8]) -> HmacSha256 {
        let mut m = HmacSha256::new_from_slice(&self.0).expect("HMAC accepts any key length");
        m.update(b"velox-bridge-v1\0");
        m.update(role.as_bytes());
        m.update(b"\0");
        m.update(server_nonce);
        m.update(client_nonce);
        m
    }

    /// `role` is `"client"` or `"server"`: a proof made for one side cannot be replayed as the other.
    pub fn prove(&self, role: &str, server_nonce: &[u8], client_nonce: &[u8]) -> Vec<u8> {
        self.mac(role, server_nonce, client_nonce).finalize().into_bytes().to_vec()
    }

    /// Constant-time verification.
    pub fn verify(&self, role: &str, server_nonce: &[u8], client_nonce: &[u8], proof: &[u8]) -> bool {
        self.mac(role, server_nonce, client_nonce).verify_slice(proof).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proofs_verify_only_for_the_right_token_role_and_nonces() {
        let t = Token::generate();
        let other = Token::generate();
        let (sn, cn) = (random_bytes::<NONCE_LEN>(), random_bytes::<NONCE_LEN>());
        let p = t.prove("client", &sn, &cn);
        assert!(t.verify("client", &sn, &cn, &p));
        assert!(!other.verify("client", &sn, &cn, &p), "different token");
        assert!(!t.verify("server", &sn, &cn, &p), "a client proof is not a server proof");
        assert!(!t.verify("client", &cn, &sn, &p), "nonces are bound in order");
        assert!(!t.verify("client", &sn, &random_bytes::<NONCE_LEN>(), &p), "fresh client nonce");
        assert!(!t.verify("client", &sn, &cn, &p[..31]) && !t.verify("client", &sn, &cn, &[]), "truncated / empty proofs");
    }

    #[test]
    fn hex_round_trip_and_rejection() {
        assert_eq!(from_hex(&to_hex(&[0, 1, 254, 255])).unwrap(), [0, 1, 254, 255]);
        assert!(from_hex("abc").is_none() && from_hex("zz").is_none() && from_hex("\u{e9}\u{e9}").is_none());
    }

    #[test]
    fn token_file_round_trip_replacement_and_debug_redaction() {
        let dir = std::env::temp_dir().join(format!("velox-token-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = Token::create_file(&dir).unwrap();
        assert_eq!(Token::read_file(&dir).unwrap(), a);
        let b = Token::create_file(&dir).unwrap();
        assert_ne!(a, b, "a new token every start");
        assert_eq!(Token::read_file(&dir).unwrap(), b);
        assert_eq!(format!("{a:?}"), "Token(..)");
        std::fs::write(dir.join(TOKEN_FILE), "not hex").unwrap();
        assert!(Token::read_file(&dir).is_err());
        assert!(Token::read_file(&dir.join("missing")).is_err());
    }
}
