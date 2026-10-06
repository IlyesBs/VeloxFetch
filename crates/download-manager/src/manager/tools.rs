//! The helper programs that video sites need, fetched by VeloxFetch itself the first time they are needed:
//!
//! * **yt-dlp** reads video pages (YouTube, Vimeo, ...);
//! * **Deno** answers the JavaScript challenges YouTube's pages contain (yt-dlp asks it);
//! * **ffmpeg** joins the separate video and audio of HD qualities (Windows; elsewhere the system's).
//!
//! ```text
//! official HTTPS source --> file + its published SHA-256 --> hash checked --> (zip: one fixed entry)
//!     --> <data folder>/tools/<fixed name>
//! ```
//! Nothing is run before its SHA-256 matched the checksum published next to it. Only fixed addresses are
//! used (ToolSources), never one from a page or the UI; files are written under fixed names in our own
//! folder; from an archive exactly one entry, found by a fixed name, is copied out, with size limits and
//! its CRC checked (no path from the archive is ever used).

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::DownloadManager;
use crate::api::ToolSources;

/// A downloaded file (an archive included) is never larger than this.
const MAX_DOWNLOAD: u64 = 400 * 1024 * 1024;
/// A program copied out of an archive is never larger than this.
const MAX_PROGRAM: u64 = 600 * 1024 * 1024;
/// Checksum lists and version files are small.
const MAX_TEXT: u64 = 256 * 1024;
/// A download that receives nothing for this long is given up (it would otherwise hold every video job).
const STALL: Duration = Duration::from_secs(60);
/// An installed yt-dlp older than this is replaced (sites change; old versions stop working).
const YTDLP_REFRESH: Duration = Duration::from_secs(14 * 24 * 3600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tool {
    YtDlp,
    Deno,
    Ffmpeg,
}

impl Tool {
    fn index(self) -> usize {
        self as usize
    }

    /// The name it is installed under, in our tools folder.
    pub(crate) fn file_name(self) -> &'static str {
        match (self, cfg!(windows)) {
            (Tool::YtDlp, true) => "yt-dlp.exe",
            (Tool::YtDlp, false) => "yt-dlp",
            (Tool::Deno, true) => "deno.exe",
            (Tool::Deno, false) => "deno",
            (Tool::Ffmpeg, true) => "ffmpeg.exe",
            (Tool::Ffmpeg, false) => "ffmpeg",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tool::YtDlp => "yt-dlp",
            Tool::Deno => "Deno",
            Tool::Ffmpeg => "ffmpeg",
        }
    }
}

/// Install state shared by every request: one installation of a tool at a time.
#[derive(Default)]
pub(crate) struct ToolsState {
    locks: [tokio::sync::Mutex<()>; 3],
    installing: [std::sync::atomic::AtomicBool; 3],
    last_error: std::sync::Mutex<[Option<String>; 3]>,
    /// Bytes received / expected for the file being fetched (progress for the UI and the installer).
    got: [std::sync::atomic::AtomicU64; 3],
    total: [std::sync::atomic::AtomicU64; 3],
}

impl ToolSources {
    /// The projects' own download servers.
    pub fn official() -> Self {
        Self {
            ytdlp: Some("https://github.com/yt-dlp/yt-dlp/releases/latest/download".into()),
            deno: Some("https://dl.deno.land".into()),
            ffmpeg: Some("https://github.com/yt-dlp/FFmpeg-Builds/releases/latest/download".into()),
            ffmpeg_fallback: Some("https://www.gyan.dev/ffmpeg/builds".into()),
            ffmpeg_unix: Some("https://ffmpeg.martin-riedl.de/redirect/latest".into()),
        }
    }
}

/// yt-dlp's release file for this computer (`None`: no official build).
pub(crate) fn ytdlp_asset() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "yt-dlp.exe",
        ("windows", "aarch64") => "yt-dlp_arm64.exe",
        ("windows", "x86") => "yt-dlp_x86.exe",
        ("macos", _) => "yt-dlp_macos",
        ("linux", "x86_64") => "yt-dlp_linux",
        ("linux", "aarch64") => "yt-dlp_linux_aarch64",
        _ => return None,
    })
}

/// Deno's release archive for this computer.
pub(crate) fn deno_asset() -> Option<String> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        _ => return None,
    };
    let os = match std::env::consts::OS {
        "windows" => "pc-windows-msvc",
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        _ => return None,
    };
    Some(format!("deno-{arch}-{os}.zip"))
}

/// ffmpeg's archive in yt-dlp's builds (Windows only: other systems install ffmpeg with their package manager).
pub(crate) fn ffmpeg_asset() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("ffmpeg-master-latest-win64-gpl.zip"),
        ("windows", "aarch64") => Some("ffmpeg-master-latest-winarm64-gpl.zip"),
        _ => None,
    }
}

/// `<os>/<arch>` of the macOS / Linux builds.
pub(crate) fn ffmpeg_unix_platform() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "macos/arm64",
        ("macos", "x86_64") => "macos/amd64",
        ("linux", "x86_64") => "linux/amd64",
        ("linux", "aarch64") => "linux/arm64",
        _ => return None,
    })
}

/// The fallback build (Windows x64).
pub(crate) fn ffmpeg_fallback_asset() -> Option<&'static str> {
    (std::env::consts::OS == "windows" && std::env::consts::ARCH == "x86_64").then_some("ffmpeg-release-essentials.zip")
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn hex_to_bytes(s: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

/// The SHA-256 in a published checksum file. With `name`: the line for that file in a list
/// (`<hash>  <name>` or `<hash> *<name>`); without: the only hash in the file (a bare hash, or the
/// `Hash : ...` line some publishers write).
pub(crate) fn parse_checksum(text: &str, name: Option<&str>) -> Option<[u8; 32]> {
    match name {
        Some(name) => text.lines().find_map(|l| {
            let mut parts = l.split_whitespace();
            let (hash, file) = (parts.next()?, parts.next_back()?);
            (is_hex64(hash) && file.trim_start_matches('*') == name).then(|| hex_to_bytes(hash)).flatten()
        }),
        None => {
            let hashes: Vec<&str> = text.split(|c: char| !c.is_ascii_hexdigit()).filter(|t| is_hex64(t)).collect();
            match hashes.as_slice() {
                [one] => hex_to_bytes(one),
                _ => None, // none, or ambiguous
            }
        }
    }
}

/// A version tag such as `v2.9.7`.
fn version_ok(v: &str) -> bool {
    v.starts_with('v') && v.len() <= 32 && v[1..].bytes().all(|b| b.is_ascii_digit() || b == b'.') && v.len() > 1
}

// ---------------------------------------------------------------------------------------------
// One entry out of a ZIP file (already checked against its published SHA-256)

fn u16_at(b: &[u8], i: usize) -> std::io::Result<u16> {
    b.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or_else(|| bad("truncated archive"))
}

fn u32_at(b: &[u8], i: usize) -> std::io::Result<u32> {
    b.get(i..i + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])).ok_or_else(|| bad("truncated archive"))
}

fn bad(why: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, why.to_string())
}

/// Copy the one entry whose name is `wanted` or ends in `/wanted` to `out`. Fails unless there is exactly
/// one such entry, it is stored or deflated, at most `max` bytes, and its size and CRC match.
pub(crate) fn extract_entry(zip: &Path, wanted: &str, out: &Path, max: u64) -> std::io::Result<()> {
    let mut f = std::fs::File::open(zip)?;
    let len = f.metadata()?.len();
    // End of central directory: in the last 64 KiB + 22 bytes.
    let tail_len = len.min(65_557);
    f.seek(SeekFrom::Start(len - tail_len))?;
    let mut tail = vec![0u8; tail_len as usize];
    f.read_exact(&mut tail)?;
    let eocd = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| tail[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
        .ok_or_else(|| bad("not a zip file"))?;
    let entries = u16_at(&tail, eocd + 10)? as usize;
    let (cd_size, cd_offset) = (u32_at(&tail, eocd + 12)? as u64, u32_at(&tail, eocd + 16)? as u64);
    if cd_offset == 0xFFFF_FFFF || cd_size > 32 * 1024 * 1024 || cd_offset + cd_size > len {
        return Err(bad("unsupported zip file"));
    }
    f.seek(SeekFrom::Start(cd_offset))?;
    let mut cd = vec![0u8; cd_size as usize];
    f.read_exact(&mut cd)?;

    let wanted_lower = wanted.to_ascii_lowercase();
    let mut found: Vec<(u16, u64, u64, u32, u64)> = Vec::new(); // method, compressed, size, crc, local header
    let mut p = 0usize;
    for _ in 0..entries {
        if u32_at(&cd, p)? != 0x0201_4b50 {
            return Err(bad("damaged zip directory"));
        }
        let method = u16_at(&cd, p + 10)?;
        let crc = u32_at(&cd, p + 16)?;
        let (csize, size) = (u32_at(&cd, p + 20)? as u64, u32_at(&cd, p + 24)? as u64);
        let (n, e, c) = (u16_at(&cd, p + 28)? as usize, u16_at(&cd, p + 30)? as usize, u16_at(&cd, p + 32)? as usize);
        let local = u32_at(&cd, p + 42)? as u64;
        let name = cd.get(p + 46..p + 46 + n).ok_or_else(|| bad("damaged zip directory"))?;
        let name = String::from_utf8_lossy(name).replace('\\', "/").to_ascii_lowercase();
        if name == wanted_lower || name.ends_with(&format!("/{wanted_lower}")) {
            found.push((method, csize, size, crc, local));
        }
        p += 46 + n + e + c;
    }
    let [(method, csize, size, crc, local)] = found.as_slice() else {
        return Err(bad(if found.is_empty() { "the program is not in the archive" } else { "the archive holds the program twice" }));
    };
    if *size > max || *csize > len || !matches!(method, 0 | 8) {
        return Err(bad("unsupported or oversized entry"));
    }
    f.seek(SeekFrom::Start(*local))?;
    let mut lh = [0u8; 30];
    f.read_exact(&mut lh)?;
    if u32_at(&lh, 0)? != 0x0403_4b50 {
        return Err(bad("damaged zip entry"));
    }
    let skip = u16_at(&lh, 26)? as i64 + u16_at(&lh, 28)? as i64;
    f.seek(SeekFrom::Current(skip))?;
    let raw = (&mut f).take(*csize);
    let mut reader: Box<dyn Read> = if *method == 8 { Box::new(flate2::read::DeflateDecoder::new(raw)) } else { Box::new(raw) };
    let mut w = std::fs::File::create(out)?;
    let (mut crc32, mut written, mut buf) = (flate2::Crc::new(), 0u64, vec![0u8; 256 * 1024]);
    loop {
        let k = reader.read(&mut buf)?;
        if k == 0 {
            break;
        }
        written += k as u64;
        if written > *size {
            return Err(bad("the entry is larger than the archive says"));
        }
        crc32.update(&buf[..k]);
        w.write_all(&buf[..k])?;
    }
    w.sync_all()?;
    if written != *size || crc32.sum() != *crc {
        return Err(bad("the extracted program is damaged"));
    }
    Ok(())
}

/// Bytes `start..=end` of `url`, appended to the part file `part` (which may already hold the first bytes
/// from an earlier try, even from before the app was closed: they are kept). A broken connection resumes
/// where it stopped (a few times). `got` counts every byte received (progress).
async fn fetch_part(
    client: &http_client::HttpClient,
    url: &url::Url,
    part: &Path,
    start: u64,
    end: u64,
    got: &std::sync::atomic::AtomicU64,
) -> Result<(), String> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    let want = end + 1 - start;
    let have = tokio::fs::metadata(part).await.map(|m| m.len()).unwrap_or(0);
    let mut file = tokio::fs::OpenOptions::new().create(true).append(true).open(part).await.map_err(|e| e.to_string())?;
    if have > want {
        file.set_len(0).await.map_err(|e| e.to_string())?;
    }
    let mut at = start + if have > want { 0 } else { have };
    got.fetch_add(at - start, Ordering::Relaxed);
    let mut tries = 0;
    while at <= end {
        let attempt = async {
            let mut body = Box::pin(client.get_range(url, at, end, None).await.map_err(|e| e.to_string())?.into_stream());
            while let Some(chunk) = tokio::time::timeout(STALL, body.next()).await.map_err(|_| "the server stopped sending".to_string())? {
                let chunk = chunk.map_err(|e| e.to_string())?;
                let room = (end + 1 - at) as usize;
                let take = chunk.len().min(room);
                file.write_all(&chunk[..take]).await.map_err(|e| e.to_string())?;
                at += take as u64;
                got.fetch_add(take as u64, Ordering::Relaxed);
                if at > end {
                    break;
                }
            }
            Ok::<(), String>(())
        }
        .await;
        match attempt {
            Ok(()) if at > end => break,
            Ok(()) | Err(_) if tries < 4 => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(500 * tries)).await;
            }
            Ok(()) => return Err("the server closed the connection early".into()),
            Err(e) => return Err(format!("download interrupted: {e}")),
        }
    }
    file.sync_all().await.map_err(|e| e.to_string())
}

/// `<dest>.<suffix>`: a part file, or the `.id` of the file the parts belong to.
fn part_name(dest: &Path, suffix: &str) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(format!(".{suffix}"));
    PathBuf::from(s)
}

/// Remove the parts (and their `.id`) kept for `dest`.
fn clear_parts(dest: &Path) {
    let _ = std::fs::remove_file(part_name(dest, "id"));
    for k in 0..16 {
        let _ = std::fs::remove_file(part_name(dest, &format!("p{k}")));
    }
}

fn make_runnable(_p: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(_p, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The manager's side

/// Where a tool's file comes from and how it is checked.
struct Plan {
    url: url::Url,
    checksum_url: url::Url,
    /// The file's name in a checksum list (`None`: the checksum file holds one hash).
    listed_as: Option<String>,
    /// Inside a ZIP: the entry to copy out.
    entry: Option<&'static str>,
    /// `url` redirects to the real file, and the checksum is `<real address>.sha256`.
    checksum_beside_target: bool,
}

fn join(base: &str, rest: &str) -> Result<url::Url, String> {
    secure(url::Url::parse(&format!("{}/{rest}", base.trim_end_matches('/'))).map_err(|_| "bad tool address".to_string())?)
}

/// Only encrypted connections, except for the local test servers.
fn secure(u: url::Url) -> Result<url::Url, String> {
    let local = matches!(u.host_str(), Some("127.0.0.1") | Some("localhost"));
    if u.scheme() != "https" && !(u.scheme() == "http" && local) {
        return Err("tools are only fetched over https".into());
    }
    Ok(u)
}

impl DownloadManager {
    /// `<data folder>/tools`: the programs VeloxFetch fetched for itself.
    pub(crate) fn tools_dir(&self) -> PathBuf {
        if let Some(d) = &self.inner.cfg.tools_dir {
            return d.clone();
        }
        self.inner.cfg.db_path.parent().map(|p| p.join("tools")).unwrap_or_else(|| PathBuf::from("tools"))
    }

    /// The installed copy of `tool`, if there is one: the user's own (fetched or refreshed by the app) first,
    /// then the one the installer put next to the app (`ManagerConfig::shared_tools_dir`).
    pub(crate) fn installed_tool(&self, tool: Tool) -> Option<PathBuf> {
        let usable = |p: PathBuf| std::fs::metadata(&p).is_ok_and(|m| m.is_file() && m.len() > 0).then_some(p);
        usable(self.tools_dir().join(tool.file_name()))
            .or_else(|| self.inner.cfg.shared_tools_dir.as_ref().and_then(|d| usable(d.join(tool.file_name()))))
    }

    fn source(&self, tool: Tool) -> Option<&str> {
        let s = &self.inner.cfg.tools;
        match tool {
            Tool::YtDlp => s.ytdlp.as_deref().filter(|_| ytdlp_asset().is_some()),
            Tool::Deno => s.deno.as_deref().filter(|_| deno_asset().is_some()),
            Tool::Ffmpeg => s
                .ffmpeg
                .as_deref()
                .filter(|_| ffmpeg_asset().is_some())
                .or(s.ffmpeg_fallback.as_deref().filter(|_| ffmpeg_fallback_asset().is_some()))
                .or(s.ffmpeg_unix.as_deref().filter(|_| ffmpeg_unix_platform().is_some())),
        }
    }

    /// VeloxFetch can fetch this tool on this computer.
    pub(crate) fn can_fetch(&self, tool: Tool) -> bool {
        self.source(tool).is_some()
    }

    pub(crate) fn tool_installing(&self, tool: Tool) -> bool {
        self.inner.tools.installing[tool.index()].load(Ordering::SeqCst)
    }

    /// Why the last attempt to fetch `tool` failed, if it did.
    pub(crate) fn tool_error(&self, tool: Tool) -> Option<String> {
        self.inner.tools.last_error.lock().unwrap_or_else(|p| p.into_inner())[tool.index()].clone()
    }

    /// The installed tool, fetching it first if it is missing (or, with `refresh`, replacing it).
    /// `None` when it cannot be fetched (no source, or the attempt failed: see `tool_error`).
    pub(crate) async fn ensure_tool(&self, tool: Tool, refresh: bool) -> Option<PathBuf> {
        if !refresh {
            if let Some(p) = self.installed_tool(tool) {
                return Some(p);
            }
        }
        self.source(tool)?;
        let _one_at_a_time = self.inner.tools.locks[tool.index()].lock().await;
        if let Some(p) = self.installed_tool(tool).filter(|_| !refresh || !self.tool_stale(tool)) {
            return Some(p); // someone else just installed (or refreshed) it
        }
        self.inner.tools.installing[tool.index()].store(true, Ordering::SeqCst);
        tracing::info!(tool = tool.label(), "fetching a video tool");
        let result = self.install(tool).await;
        self.inner.tools.installing[tool.index()].store(false, Ordering::SeqCst);
        let mut errors = self.inner.tools.last_error.lock().unwrap_or_else(|p| p.into_inner());
        match result {
            Ok(p) => {
                tracing::info!(tool = tool.label(), "video tool installed");
                errors[tool.index()] = None;
                Some(p)
            }
            Err(e) => {
                tracing::warn!(tool = tool.label(), error = %e, "could not fetch a video tool");
                errors[tool.index()] = Some(e);
                // A refresh that failed keeps the copy that was there.
                self.installed_tool(tool)
            }
        }
    }

    /// Start fetching `tool` without waiting for it.
    pub(crate) fn fetch_in_background(&self, tool: Tool, refresh: bool) {
        if !self.can_fetch(tool) || self.tool_installing(tool) || (!refresh && self.installed_tool(tool).is_some()) {
            return;
        }
        let mgr = self.clone();
        tokio::spawn(async move {
            let _ = mgr.ensure_tool(tool, refresh).await;
        });
    }

    /// Our yt-dlp is old enough to be replaced.
    pub(crate) fn tool_stale(&self, tool: Tool) -> bool {
        tool == Tool::YtDlp
            && self
                .installed_tool(tool)
                .and_then(|p| std::fs::metadata(p).ok()?.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > YTDLP_REFRESH)
    }

    /// Where to get `tool`, in the order to try.
    async fn plans(&self, tool: Tool) -> Result<Vec<Plan>, String> {
        if tool != Tool::Ffmpeg {
            return Ok(vec![self.plan(tool).await?]);
        }
        let s = &self.inner.cfg.tools;
        let mut plans = Vec::new();
        if let (Some(base), Some(asset)) = (s.ffmpeg.as_deref(), ffmpeg_asset()) {
            plans.push(Plan {
                url: join(base, asset)?,
                checksum_url: join(base, "checksums.sha256")?,
                listed_as: Some(asset.into()),
                entry: Some("bin/ffmpeg.exe"),
                checksum_beside_target: false,
            });
        }
        if let (Some(base), Some(asset)) = (s.ffmpeg_fallback.as_deref(), ffmpeg_fallback_asset()) {
            plans.push(Plan {
                url: join(base, asset)?,
                checksum_url: join(base, &format!("{asset}.sha256"))?,
                listed_as: None,
                entry: Some("bin/ffmpeg.exe"),
                checksum_beside_target: false,
            });
        }
        if let (Some(base), Some(platform)) = (s.ffmpeg_unix.as_deref(), ffmpeg_unix_platform()) {
            for channel in ["release", "snapshot"] {
                let url = join(base, &format!("{platform}/{channel}/ffmpeg.zip"))?;
                plans.push(Plan { checksum_url: url.clone(), url, listed_as: None, entry: Some("ffmpeg"), checksum_beside_target: true });
            }
        }
        Ok(plans)
    }

    async fn plan(&self, tool: Tool) -> Result<Plan, String> {
        let base = self.source(tool).ok_or("no source")?;
        Ok(match tool {
            Tool::YtDlp => {
                let asset = ytdlp_asset().ok_or("no build for this computer")?;
                Plan {
                    url: join(base, asset)?,
                    checksum_url: join(base, "SHA2-256SUMS")?,
                    listed_as: Some(asset.into()),
                    entry: None,
                    checksum_beside_target: false,
                }
            }
            Tool::Deno => {
                let asset = deno_asset().ok_or("no build for this computer")?;
                let version = String::from_utf8_lossy(&self.fetch_text(&join(base, "release-latest.txt")?).await?).trim().to_string();
                if !version_ok(&version) {
                    return Err("unexpected Deno version".into());
                }
                let url = join(base, &format!("release/{version}/{asset}"))?;
                let checksum_url = join(base, &format!("release/{version}/{asset}.sha256sum"))?;
                Plan { url, checksum_url, listed_as: None, entry: Some(Tool::Deno.file_name()), checksum_beside_target: false }
            }
            Tool::Ffmpeg => return Err("ffmpeg has several sources (see plans)".into()),
        })
    }

    async fn fetch_text(&self, url: &url::Url) -> Result<Vec<u8>, String> {
        let mut resp = self.media_client().get(url).await.map_err(|e| format!("{url}: {e}"))?;
        let mut out: Vec<u8> = Vec::new();
        while let Some(chunk) = tokio::time::timeout(STALL, resp.chunk())
            .await
            .map_err(|_| "the server stopped sending".to_string())?
            .map_err(|e| format!("download interrupted: {e}"))?
        {
            out.extend_from_slice(&chunk);
            if out.len() as u64 > MAX_TEXT {
                return Err("checksum file too large".into());
            }
        }
        Ok(out)
    }

    async fn install(&self, tool: Tool) -> Result<PathBuf, String> {
        let dir = self.tools_dir();
        tokio::fs::create_dir_all(&dir).await.map_err(|e| format!("cannot create the tools folder: {e}"))?;
        let mut last = String::from("no source");
        // A dropped connection is common on slow lines: try every source again, a little later (the parts
        // already downloaded are kept, so a new round continues them).
        for round in 0..3u64 {
            // A wrong or missing checksum is not a network hiccup: trying again would not help.
            if round > 0 && last.contains("checksum") {
                break;
            }
            if round > 0 {
                tokio::time::sleep(Duration::from_secs(5 * round)).await;
            }
            let plans = match self.plans(tool).await {
                Ok(p) => p,
                Err(e) => {
                    last = e;
                    continue;
                }
            };
            for plan in plans {
                match self.install_from(tool, &dir, &plan).await {
                    Ok(p) => return Ok(p),
                    Err(e) => {
                        tracing::warn!(tool = tool.label(), source = %plan.url.host_str().unwrap_or(""), round, error = %e, "a video tool source failed");
                        last = e;
                    }
                }
            }
        }
        Err(last)
    }

    async fn install_from(&self, tool: Tool, dir: &Path, plan: &Plan) -> Result<PathBuf, String> {
        let resolved;
        let plan = if plan.checksum_beside_target {
            // Follow the "latest" address to the build it names, and take that build's checksum.
            let info = self.media_client().probe(&plan.url).await.map_err(|e| format!("{}: {e}", plan.url))?;
            let target = secure(url::Url::parse(&info.final_url).map_err(|_| "bad address".to_string())?)?;
            let checksum_url = url::Url::parse(&format!("{}.sha256", info.final_url)).map_err(|_| "bad address".to_string())?;
            resolved = Plan { url: target, checksum_url, listed_as: None, entry: plan.entry, checksum_beside_target: false };
            &resolved
        } else {
            plan
        };
        let sums = self.fetch_text(&plan.checksum_url).await?;
        let expected =
            parse_checksum(&String::from_utf8_lossy(&sums), plan.listed_as.as_deref()).ok_or("no checksum published for the file")?;
        let download = dir.join(format!(".{}.download", tool.label()));
        let fresh = dir.join(format!(".{}.new", tool.label()));
        let result = async {
            let got = self.fetch_file(tool, &plan.url, &download).await?;
            if got != expected {
                return Err("the file does not match its published checksum".to_string());
            }
            let (download2, fresh2, entry) = (download.clone(), fresh.clone(), plan.entry);
            tokio::task::spawn_blocking(move || -> std::io::Result<()> {
                match entry {
                    Some(e) => extract_entry(&download2, e, &fresh2, MAX_PROGRAM)?,
                    None => std::fs::rename(&download2, &fresh2)?,
                }
                make_runnable(&fresh2)
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("cannot unpack it: {e}"))?;
            let target = dir.join(tool.file_name());
            std::fs::rename(&fresh, &target).map_err(|e| format!("cannot put it in place: {e}"))?;
            Ok(target)
        }
        .await;
        let _ = std::fs::remove_file(&download);
        let _ = std::fs::remove_file(&fresh);
        result
    }

    /// Download `url` to `dest` and return its SHA-256. Large files that the server serves in ranges come
    /// over `PARTS` connections (some servers limit each connection), each into its own part file next to
    /// `dest`. The parts are kept when the download stops (the app closed, the network dropped): the next
    /// try continues them, as long as the server still describes the same file (`<dest>.id`).
    async fn fetch_file(&self, tool: Tool, url: &url::Url, dest: &Path) -> Result<[u8; 32], String> {
        const PARTS: u64 = 8;
        const MIN_PARALLEL: u64 = 8 * 1024 * 1024;
        let client = self.media_client();
        let info = client.probe(url).await.ok();
        let parallel = info.as_ref().filter(|i| i.supports_parallel()).and_then(|i| {
            let identity = format!("{url}\n{}\n{}", i.content_length?, i.etag.as_deref().or(i.last_modified.as_deref()).unwrap_or(""));
            Some((i.content_length?, url::Url::parse(&i.final_url).ok()?, identity))
        });
        let Some((len, at, identity)) = parallel.filter(|(len, _, _)| *len >= MIN_PARALLEL) else {
            clear_parts(dest);
            return self.fetch_whole(tool, url, dest).await;
        };
        if len > MAX_DOWNLOAD {
            return Err("the file is unexpectedly large".into());
        }
        let id_file = part_name(dest, "id");
        if std::fs::read_to_string(&id_file).ok().as_deref() != Some(identity.as_str()) {
            clear_parts(dest); // another file now (or the first try): start over
            std::fs::write(&id_file, &identity).map_err(|e| e.to_string())?;
        }
        let state = &self.inner.tools;
        state.total[tool.index()].store(len, Ordering::Relaxed);
        state.got[tool.index()].store(0, Ordering::Relaxed);
        let step = len.div_ceil(PARTS);
        let mut ranges = Vec::new();
        let mut parts = Vec::new();
        for k in 0..PARTS {
            let (start, end) = (k * step, ((k + 1) * step).min(len) - 1);
            if start > end {
                continue;
            }
            let part = part_name(dest, &format!("p{k}"));
            ranges.push(part.clone());
            let (client, at, mgr) = (client.clone(), at.clone(), self.clone());
            parts.push(tokio::spawn(async move { fetch_part(&client, &at, &part, start, end, &mgr.inner.tools.got[tool.index()]).await }));
        }
        for p in parts {
            p.await.map_err(|e| e.to_string())??;
        }
        // Join the parts in order into `dest`, hashing on the way.
        let dest = dest.to_path_buf();
        let hash = tokio::task::spawn_blocking(move || -> std::io::Result<[u8; 32]> {
            let mut out = std::fs::File::create(&dest)?;
            let (mut h, mut buf) = (Sha256::new(), vec![0u8; 1 << 20]);
            for part in &ranges {
                let mut f = std::fs::File::open(part)?;
                loop {
                    let n = f.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    h.update(&buf[..n]);
                    out.write_all(&buf[..n])?;
                }
            }
            out.sync_all()?;
            clear_parts(&dest);
            Ok(h.finalize().into())
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        Ok(hash)
    }

    /// How far the current download of `tool` is: (received, total), when it is being fetched.
    pub(crate) fn tool_progress(&self, tool: Tool) -> Option<(u64, u64)> {
        let s = &self.inner.tools;
        (self.tool_installing(tool)).then(|| (s.got[tool.index()].load(Ordering::Relaxed), s.total[tool.index()].load(Ordering::Relaxed)))
    }

    /// One plain GET, hashed on the way.
    async fn fetch_whole(&self, tool: Tool, url: &url::Url, dest: &Path) -> Result<[u8; 32], String> {
        use tokio::io::AsyncWriteExt;
        let state = &self.inner.tools;
        state.got[tool.index()].store(0, Ordering::Relaxed);
        state.total[tool.index()].store(0, Ordering::Relaxed);
        let mut resp = self.media_client().get(url).await.map_err(|e| format!("{url}: {e}"))?;
        if let Some(n) = resp.content_length() {
            state.total[tool.index()].store(n, Ordering::Relaxed);
        }
        let mut file = tokio::fs::File::create(dest).await.map_err(|e| e.to_string())?;
        let (mut hash, mut total) = (Sha256::new(), 0u64);
        while let Some(chunk) = tokio::time::timeout(STALL, resp.chunk())
            .await
            .map_err(|_| "the server stopped sending".to_string())?
            .map_err(|e| format!("download interrupted: {e}"))?
        {
            total += chunk.len() as u64;
            state.got[tool.index()].fetch_add(chunk.len() as u64, Ordering::Relaxed);
            if total > MAX_DOWNLOAD {
                return Err("the file is unexpectedly large".to_string());
            }
            hash.update(&chunk);
            file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        }
        file.sync_all().await.map_err(|e| e.to_string())?;
        Ok(hash.finalize().into())
    }

    /// Which tools are there, and why one could not be fetched.
    pub fn video_tools(&self) -> crate::api::VideoToolsDto {
        let status = |t: Tool, found: bool| {
            let (received, total) = self.tool_progress(t).unwrap_or((0, 0));
            crate::api::ToolStatus {
                installed: found,
                fetching: self.tool_installing(t),
                can_fetch: self.can_fetch(t),
                error: self.tool_error(t),
                received,
                total,
            }
        };
        crate::api::VideoToolsDto {
            ytdlp: status(Tool::YtDlp, self.ytdlp().is_some()),
            deno: status(Tool::Deno, self.installed_tool(Tool::Deno).is_some()),
            ffmpeg: status(Tool::Ffmpeg, self.ffmpeg().is_some()),
        }
    }

    /// Start fetching every missing tool (and refresh an old yt-dlp) without waiting.
    pub fn prefetch_video_tools(&self) {
        if self.ytdlp().is_none() {
            self.fetch_in_background(Tool::YtDlp, false);
        } else if self.tool_stale(Tool::YtDlp) {
            self.fetch_in_background(Tool::YtDlp, true);
        }
        self.fetch_in_background(Tool::Deno, false);
        if self.ffmpeg().is_none() {
            self.fetch_in_background(Tool::Ffmpeg, false);
        }
    }

    /// Fetch every missing tool now (and wait for them).
    pub async fn fetch_video_tools(&self) -> crate::api::VideoToolsDto {
        let need = |t: Tool, found: bool| async move {
            if !found {
                self.ensure_tool(t, false).await;
            }
        };
        tokio::join!(
            need(Tool::YtDlp, self.ytdlp().is_some()),
            need(Tool::Deno, self.installed_tool(Tool::Deno).is_some()),
            need(Tool::Ffmpeg, self.ffmpeg().is_some())
        );
        self.video_tools()
    }

    /// Text for a video page that cannot be read because yt-dlp is missing.
    pub(crate) fn no_ytdlp_message(&self) -> String {
        // The reason (with the program's name) is in the log; the user gets what to do.
        match self.tool_error(Tool::YtDlp) {
            Some(_) => "VeloxFetch could not download the required dependencies for video sites. Check the internet connection \
                        and try again, or press Download in Settings > Required dependencies."
                .to_string(),
            None => "Videos from this site need the required dependencies. Press Download in Settings > Required dependencies, \
                     then try again."
                .to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_are_read_from_lists_and_single_hash_files() {
        let h = "a0c3101b4158d1dfb7d6a78a7bf0f3de80c96bb423c152beec8beb22786f2238";
        let list = format!("{}  yt-dlp_linux\n{h}  yt-dlp.exe\n{} *yt-dlp_macos\n", "1".repeat(64), "2".repeat(64));
        assert_eq!(parse_checksum(&list, Some("yt-dlp.exe")), hex_to_bytes(h));
        assert_eq!(parse_checksum(&list, Some("yt-dlp_macos")), hex_to_bytes(&"2".repeat(64)));
        assert_eq!(parse_checksum(&list, Some("other.exe")), None);
        // A bare hash, and the "Hash : ..." form, both upper and lower case.
        assert_eq!(parse_checksum(&format!("{h}\n"), None), hex_to_bytes(h));
        let ps = format!("\r\nAlgorithm : SHA256\r\nHash      : {}\r\nPath      : C:\\a\\deno.zip\r\n", h.to_uppercase());
        assert_eq!(parse_checksum(&ps, None), hex_to_bytes(h));
        // Two hashes without a name to pick one: refused.
        assert_eq!(parse_checksum(&list, None), None);
        assert_eq!(parse_checksum("not a hash", None), None);
    }

    #[test]
    fn versions_and_addresses_are_checked() {
        assert!(version_ok("v2.9.7"));
        for bad in ["2.9.7", "v", "v2.9.7/../x", "v2;rm", ""] {
            assert!(!version_ok(bad), "{bad}");
        }
        assert!(join("https://dl.deno.land", "release-latest.txt").is_ok());
        assert!(join("http://127.0.0.1:9000/x", "a").is_ok(), "local test servers");
        assert!(join("http://example.com", "a").is_err(), "never plain http on the internet");
    }

    /// A ZIP built by hand: (name, data, deflated).
    fn zip_of(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
        use flate2::write::DeflateEncoder;
        let (mut out, mut central) = (Vec::new(), Vec::new());
        for (name, data, deflate) in files {
            let body = if *deflate {
                let mut e = DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            } else {
                data.to_vec()
            };
            let mut crc = flate2::Crc::new();
            crc.update(data);
            let method: u16 = if *deflate { 8 } else { 0 };
            let offset = out.len() as u32;
            let mut common = Vec::new();
            common.extend_from_slice(&method.to_le_bytes());
            common.extend_from_slice(&[0, 0, 0, 0]); // time, date
            common.extend_from_slice(&crc.sum().to_le_bytes());
            common.extend_from_slice(&(body.len() as u32).to_le_bytes());
            common.extend_from_slice(&(data.len() as u32).to_le_bytes());
            common.extend_from_slice(&(name.len() as u16).to_le_bytes());
            common.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 0]);
            out.extend_from_slice(&common);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&body);
            central.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02, 20, 0, 20, 0, 0, 0]);
            central.extend_from_slice(&common);
            central.extend_from_slice(&[0; 10]); // comment length, disk, internal and external attributes
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let cd_offset = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0]);
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    #[test]
    fn exactly_one_named_entry_is_copied_out_of_a_zip() {
        let dir = std::env::temp_dir().join(format!("velox-zip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let program: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let zip = dir.join("a.zip");
        std::fs::write(
            &zip,
            zip_of(&[
                ("ffmpeg-9.0/README.txt", b"hello", false),
                ("ffmpeg-9.0/bin/ffmpeg.exe", &program, true),
                ("../evil.exe", b"x", false),
            ]),
        )
        .unwrap();
        let out = dir.join("ffmpeg.exe");
        extract_entry(&zip, "bin/ffmpeg.exe", &out, 1 << 20).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), program);
        // Stored entries too; names are matched whole, and paths in the archive are never used.
        extract_entry(&zip, "README.txt", &dir.join("r"), 100).unwrap();
        assert!(extract_entry(&zip, "missing.exe", &dir.join("m"), 1 << 20).is_err());
        assert!(extract_entry(&zip, "fmpeg.exe", &dir.join("m"), 1 << 20).is_err(), "no partial-name match");
        assert!(extract_entry(&zip, "bin/ffmpeg.exe", &dir.join("small"), 1000).is_err(), "size limit");
        assert!(!dir.parent().unwrap().join("evil.exe").exists());
        // Two entries of that name: refused.
        std::fs::write(&zip, zip_of(&[("a/deno.exe", b"1", false), ("b/deno.exe", b"2", false)])).unwrap();
        assert!(extract_entry(&zip, "deno.exe", &dir.join("d"), 100).is_err());
        // A damaged body: the CRC catches it.
        let mut bytes = zip_of(&[("deno.exe", &program, false)]);
        bytes[40] ^= 0xff;
        std::fs::write(&zip, bytes).unwrap();
        assert!(extract_entry(&zip, "deno.exe", &dir.join("d2"), 1 << 20).is_err());
        assert!(extract_entry(&dir.join("r"), "deno.exe", &dir.join("d3"), 100).is_err(), "not a zip");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
