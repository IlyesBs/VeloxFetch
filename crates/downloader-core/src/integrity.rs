//! File integrity: SHA-256 / SHA-512 verification against a caller-supplied checksum.
//!
//! Trust model: the expected checksum must come from somewhere *more trusted than the
//! file's own server* (a signed release page, a package manifest, the user). The engine
//! never derives a checksum from the same response it is validating.
//!
//! A publisher's checksum list (`SHA256SUMS`, `<file>.sha256`... next to the file on the same site, issue #6 of the
//! feature list) is a separate, weaker source: it proves the file was not damaged or cut short on its way, not that
//! the site is honest, and is reported as such ([`find_in_list`], [`PublisherLookup`]).

use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};

use crate::error::{DownloadError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HashAlgo {
    Sha256,
    Sha512,
}

impl HashAlgo {
    fn hex_len(self) -> usize {
        match self {
            Self::Sha256 => 64,
            Self::Sha512 => 128,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checksum {
    pub algo: HashAlgo,
    /// Lowercase hex digest.
    pub hex: String,
}

impl Checksum {
    /// Parse `sha256:<hex>` or `sha512:<hex>`.
    pub fn parse(s: &str) -> std::result::Result<Self, String> {
        let (a, h) = s.split_once(':').ok_or("expected `sha256:<hex>` or `sha512:<hex>`")?;
        let algo = match a.trim().to_ascii_lowercase().as_str() {
            "sha256" | "sha-256" => HashAlgo::Sha256,
            "sha512" | "sha-512" => HashAlgo::Sha512,
            other => return Err(format!("unsupported algorithm `{other}`")),
        };
        let hex = h.trim().to_ascii_lowercase();
        if hex.len() != algo.hex_len() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("{} digest must be {} hex characters", algo.name(), algo.hex_len()));
        }
        Ok(Self { algo, hex })
    }
}

impl std::fmt::Display for Checksum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.algo.name(), self.hex)
    }
}

/// Lines read from a checksum list at most (a real one has a few hundred).
const MAX_LIST_LINES: usize = 20_000;

/// The checksum a publisher's list gives for `file_name`: GNU lines (`<hex>  name`, `<hex> *name`, `./name`, a path
/// ending in `/name`) and BSD lines (`SHA256 (name) = <hex>`), SHA-256 or SHA-512 by the digest's length. With
/// `single` (a list made for this one file, `<file>.sha256`) a lone digest counts too. `None` when the name is not
/// listed, or listed with two different digests of the same kind (ambiguous: never guessed). Signature armour and
/// other lines are skipped; the strongest digest found wins (SHA-512 over SHA-256).
pub fn find_in_list(text: &str, file_name: &str, single: bool) -> Option<Checksum> {
    if file_name.is_empty() || file_name.contains(['/', '\\', '\n', '\r']) {
        return None;
    }
    let digest = |h: &str| -> Option<Checksum> {
        let algo = match h.len() {
            64 => HashAlgo::Sha256,
            128 => HashAlgo::Sha512,
            _ => return None,
        };
        h.bytes().all(|b| b.is_ascii_hexdigit()).then(|| Checksum { algo, hex: h.to_ascii_lowercase() })
    };
    let names = |n: &str| {
        let n = n.trim_end();
        let n = n.strip_prefix("./").unwrap_or(n);
        n == file_name || n.rsplit_once('/').is_some_and(|(_, last)| last == file_name)
    };
    let mut found: Vec<Checksum> = Vec::new();
    let mut lone: Vec<Checksum> = Vec::new();
    for line in text.lines().take(MAX_LIST_LINES) {
        let line = line.trim();
        // BSD: `SHA256 (name) = hex`.
        if let Some((head, hex)) = line.rsplit_once(") = ") {
            if let Some((tag, name)) = head.split_once(" (") {
                if let Some(c) = digest(hex.trim()) {
                    let tag_ok = matches!(
                        (tag.trim().to_ascii_uppercase().as_str(), c.algo),
                        ("SHA256" | "SHA2-256", HashAlgo::Sha256) | ("SHA512" | "SHA2-512", HashAlgo::Sha512)
                    );
                    if tag_ok && names(name) {
                        found.push(c);
                    }
                }
                continue;
            }
        }
        // GNU: `hex  name` / `hex *name`, or a lone `hex`.
        let (hex, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let Some(c) = digest(hex) else { continue };
        let rest = rest.trim_start();
        let name = rest.strip_prefix('*').unwrap_or(rest);
        if name.is_empty() {
            lone.push(c);
        } else if names(name) {
            found.push(c);
        }
    }
    if found.is_empty() && single {
        found = lone;
    }
    let pick = |algo: HashAlgo| -> std::result::Result<Option<Checksum>, ()> {
        let mut of: Vec<&Checksum> = found.iter().filter(|c| c.algo == algo).collect();
        of.sort_by(|a, b| a.hex.cmp(&b.hex));
        of.dedup_by(|a, b| a.hex == b.hex);
        match of.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some((*one).clone())),
            _ => Err(()),
        }
    };
    match (pick(HashAlgo::Sha512), pick(HashAlgo::Sha256)) {
        (Err(()), _) | (_, Err(())) => None,
        (Ok(Some(c)), _) | (Ok(None), Ok(Some(c))) => Some(c),
        _ => None,
    }
}

/// The publisher's checksum for a running download, looked up next to the file while it downloads (the manager
/// does it; the engine waits for the answer before it finishes the file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublisherLookup {
    Looking,
    /// `list`: the list's name as shown (`SHA256SUMS`), never its address.
    Found {
        checksum: Checksum,
        list: String,
    },
    None,
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hash_reader<D: Digest>(mut r: impl Read) -> std::io::Result<String> {
    let mut h = D::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(to_hex(&h.finalize()))
}

/// Stream-hash a file (constant memory) off the async runtime's worker threads.
pub async fn compute_file(path: &Path, algo: HashAlgo) -> Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let f = std::fs::File::open(path)?;
        match algo {
            HashAlgo::Sha256 => hash_reader::<Sha256>(f),
            HashAlgo::Sha512 => hash_reader::<Sha512>(f),
        }
    })
    .await
    .map_err(|e| DownloadError::Internal(e.to_string()))?
    .map_err(DownloadError::from)
}

pub async fn verify_file(path: &Path, expected: &Checksum) -> Result<()> {
    let actual = compute_file(path, expected.algo).await?;
    if actual == expected.hex {
        Ok(())
    } else {
        Err(DownloadError::ChecksumMismatch { expected: expected.to_string(), actual: format!("{}:{actual}", expected.algo.name()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABC256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn parse_rules() {
        assert!(Checksum::parse(&format!("sha256:{ABC256}")).is_ok());
        assert!(Checksum::parse(&format!("SHA256:{}", ABC256.to_uppercase())).is_ok());
        assert!(Checksum::parse("sha256:abc").is_err());
        assert!(Checksum::parse("md5:d41d8cd98f00b204e9800998ecf8427e").is_err());
        assert!(Checksum::parse(&format!("sha256:{}", "z".repeat(64))).is_err());
    }

    #[test]
    fn publisher_lists_in_every_usual_format() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let c = "c".repeat(128);
        let gnu = format!("{b}  other.iso\n{a}  debian-12.iso\n");
        assert_eq!(find_in_list(&gnu, "debian-12.iso", false).unwrap().hex, a);
        assert_eq!(find_in_list(&format!("{a} *debian-12.iso\r\n"), "debian-12.iso", false).unwrap().hex, a);
        assert_eq!(find_in_list(&format!("{a}  ./x/debian-12.iso"), "debian-12.iso", false).unwrap().hex, a);
        assert_eq!(find_in_list(&format!("SHA256 (debian-12.iso) = {a}"), "debian-12.iso", false).unwrap().hex, a);
        let s512 = find_in_list(&format!("SHA512 (f.zip) = {}", c.to_uppercase()), "f.zip", false).unwrap();
        assert_eq!((s512.algo, s512.hex.as_str()), (HashAlgo::Sha512, c.as_str()));
        // A file name with spaces; a signed (clearsigned) list.
        assert_eq!(find_in_list(&format!("{a}  my file (1).iso"), "my file (1).iso", false).unwrap().hex, a);
        let signed = format!("-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\n{a}  linux.tar.xz\n-----BEGIN PGP SIGNATURE-----\n{b}\n");
        assert_eq!(find_in_list(&signed, "linux.tar.xz", false).unwrap().hex, a);
        // A list made for this one file: a lone digest.
        assert_eq!(find_in_list(&format!("{a}\n"), "f.iso", true).unwrap().hex, a);
        assert!(find_in_list(&format!("{a}\n"), "f.iso", false).is_none());
        // SHA-512 wins over SHA-256.
        assert_eq!(find_in_list(&format!("{a}  f.iso\n{c}  f.iso"), "f.iso", false).unwrap().algo, HashAlgo::Sha512);
    }

    #[test]
    fn publisher_lists_never_guess() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        // Not listed, a name that only contains it, md5 / sha1, two different digests, a web page.
        assert!(find_in_list(&format!("{a}  other.iso"), "f.iso", false).is_none());
        assert!(find_in_list(&format!("{a}  f.iso.sig"), "f.iso", false).is_none());
        assert!(find_in_list(&format!("{a}  old-f.iso"), "f.iso", false).is_none());
        assert!(find_in_list("d41d8cd98f00b204e9800998ecf8427e  f.iso", "f.iso", false).is_none());
        assert!(find_in_list(&format!("{a}  f.iso\n{b}  f.iso"), "f.iso", false).is_none());
        assert!(find_in_list(&format!("{a}  f.iso\n{a}  f.iso"), "f.iso", false).is_some(), "the same digest twice is fine");
        assert!(find_in_list(&format!("{a}\n{b}\n"), "f.iso", true).is_none());
        assert!(find_in_list("<html><body>Not found</body></html>", "f.iso", true).is_none());
        assert!(find_in_list(&format!("SHA256 (f.iso) = {}", "c".repeat(128)), "f.iso", false).is_none(), "tag and length must agree");
        // A name with a path in it is refused (only the file's own name is looked for).
        assert!(find_in_list(&format!("{a}  ../f.iso"), "../f.iso", false).is_none());
        // Huge lists are read only so far.
        let big = format!("{}{a}  f.iso", format!("{b}  x\n").repeat(MAX_LIST_LINES));
        assert!(find_in_list(&big, "f.iso", false).is_none());
    }

    #[tokio::test]
    async fn known_vectors() {
        let p = std::env::temp_dir().join(format!("velox-hash-{}.bin", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(compute_file(&p, HashAlgo::Sha256).await.unwrap(), ABC256);
        let s512 = compute_file(&p, HashAlgo::Sha512).await.unwrap();
        assert!(s512.starts_with("ddaf35a193617aba"));
        verify_file(&p, &Checksum::parse(&format!("sha256:{ABC256}")).unwrap()).await.unwrap();
        let bad = Checksum::parse(&format!("sha256:{}", "0".repeat(64))).unwrap();
        assert!(matches!(verify_file(&p, &bad).await, Err(DownloadError::ChecksumMismatch { .. })));
        let _ = std::fs::remove_file(p);
    }
}
