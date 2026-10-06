//! File integrity: SHA-256 / SHA-512 verification against a caller-supplied checksum.
//!
//! Trust model: the expected checksum must come from somewhere *more trusted than the
//! file's own server* (a signed release page, a package manifest, the user). The engine
//! never derives a checksum from the same response it is validating.

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
