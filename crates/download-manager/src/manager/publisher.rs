//! The publisher's checksum (feature #6): many sites publish a checksum list next to their files (`SHA256SUMS` for
//! Ubuntu and Debian, `sha256sums.asc` for the Linux kernel, `SHASUMS256.txt` for Node, `<file>.sha256`...). While a
//! download runs, its folder on the site is asked for those lists (the usual names only, a handful of small requests at
//! once; a folder's shared lists once for all its files; at most [`LOOKUP_BUDGET`] in all); the line for this exact
//! file name is the publisher's checksum, and the engine checks the
//! file against it before it is finished (`DownloadControl::set_publisher`). The Add dialog's check looks too, so the
//! user knows beforehand; the answer is kept a few minutes so the download does not ask again.
//!
//! What it proves: the file was not damaged or cut short on its way (a broken mirror, a bad connection, a disk
//! error). The list comes from the same site, so it cannot prove the site itself is honest: it is shown as "matches
//! the publisher's checksum", never as "safe". Signatures (`.asc`, `.gpg`) are not checked.
//!
//! Safety: `https` links only; the lists are asked from the same host, through the same protected client as every
//! download (proxy, local-address protection), with no cookie, sign-in, referrer or custom header; an answer that
//! redirects to another host, is a web page, or is larger than [`MAX_LIST`] is ignored. Nothing is written but the
//! result; only the list's *name* is shown and kept, never its address.

use std::collections::HashMap;

use downloader_core::{find_in_list, PublisherLookup};
use url::Url;

use super::*;

/// The setting: look for the publisher's checksum (default: yes).
const SETTING_KEY: &str = "downloads.publisher_checksum";
/// The whole lookup (every list asked) takes at most this long.
pub(super) const LOOKUP_BUDGET: Duration = Duration::from_secs(10);
/// One list's request.
const LIST_TIMEOUT: Duration = Duration::from_secs(5);
/// A list larger than this is not read (a real one is a few KiB).
const MAX_LIST: usize = 1024 * 1024;
/// An answer is kept this long (the Add dialog's check, then the download itself).
const KEEP_FOR: Duration = Duration::from_secs(10 * 60);
/// Answers kept at most.
const KEEP_MAX: usize = 64;
/// Folders whose lists are kept at most, and their lists' size in all (the oldest go first).
const FOLDERS_MAX: usize = 16;
const FOLDERS_BYTES: usize = 8 * 1024 * 1024;
/// Files in a row with no list of their own in a folder after which their own lists are no longer asked there (a
/// batch of a hundred files from a site without lists sends a handful of requests, not hundreds).
const FILE_LIST_MISSES: u32 = 2;

/// Characters escaped in a list's name (everything but letters, digits and `.-_~`).
const NAME_ESCAPE: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC.remove(b'.').remove(b'-').remove(b'_').remove(b'~');

/// Lists made for one file (`<file>.sha256`): a lone digest counts.
const FILE_LISTS: &[&str] = &["sha256", "sha512", "sha256sum", "sha512sum"];
/// Lists of a whole folder, the usual names, the strongest first.
const FOLDER_LISTS: &[&str] =
    &["SHA512SUMS", "SHA256SUMS", "SHA256SUMS.txt", "sha256sums.txt", "sha256sum.txt", "SHASUMS256.txt", "sha256sums.asc"];

#[derive(Default)]
pub(super) struct Lookups {
    /// By link (without its query): what was found, and when.
    answers: Mutex<HashMap<String, (Instant, PublisherLookup)>>,
    /// Running downloads: what their lookup found so far (for their card).
    running: Mutex<HashMap<i64, PublisherLookup>>,
    /// By folder: its shared lists (asked once for every file of the folder) and how many files in a row had no
    /// list of their own.
    folders: Mutex<HashMap<String, Folder>>,
    /// One lookup at a time per folder (the files of a batch share its lists instead of all asking for them).
    asking: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

struct Folder {
    at: Instant,
    /// Each shared list's name and text (`None`: missing, not a list, too large or too slow).
    lists: Vec<(String, Option<Arc<String>>)>,
    file_list_misses: u32,
}

/// The file's own name in the link (the last part of its path, decoded), when it has one.
pub(super) fn link_file_name(url: &Url) -> Option<String> {
    let last = url.path_segments()?.next_back()?;
    let name = percent_encoding::percent_decode_str(last).decode_utf8().ok()?.to_string();
    (!name.is_empty() && name.len() <= 255 && !name.contains(['/', '\\', '\0']) && name != "." && name != "..").then_some(name)
}

/// The lists to ask for `url`'s file, in order: `(address, name shown, made for this one file)`. None for a link
/// that is not `https` or has no file name.
pub(super) fn candidates(url: &Url, over_http: bool) -> Vec<(Url, String, bool)> {
    let Some(file) = link_file_name(url) else { return Vec::new() };
    if !(url.scheme() == "https" || over_http && url.scheme() == "http") || url.host_str().is_none() {
        return Vec::new();
    }
    let mut folder = url.clone();
    folder.set_query(None);
    folder.set_fragment(None);
    let at = |name: &str| folder.join(&percent_encoding::utf8_percent_encode(name, NAME_ESCAPE).to_string()).ok();
    let mut out = Vec::new();
    for ext in FILE_LISTS {
        let name = format!("{file}.{ext}");
        if let Some(u) = at(&name) {
            out.push((u, name, true));
        }
    }
    for name in FOLDER_LISTS {
        if let Some(u) = at(name) {
            out.push((u, (*name).to_string(), false));
        }
    }
    out
}

/// The folder the file is in (its address without the file name, query or fragment).
fn folder_key(url: &Url) -> String {
    let mut u = url.clone();
    u.set_query(None);
    u.set_fragment(None);
    u.join("./").map_or_else(|_| u.to_string(), |f| f.to_string())
}

fn key(url: &Url) -> String {
    let mut u = url.clone();
    u.set_query(None);
    u.set_fragment(None);
    u.to_string()
}

impl DownloadManager {
    /// Look for the publisher's checksum (Settings > Downloads; default: yes).
    pub fn publisher_check(&self) -> bool {
        self.inner.store.get_setting(SETTING_KEY).ok().flatten().is_none_or(|v| v != "false")
    }

    pub async fn set_publisher_check(&self, on: bool) -> Result<bool, ManagerError> {
        self.put_setting(SETTING_KEY, if on { "true" } else { "false" }.to_string()).await?;
        Ok(on)
    }

    /// What the lookup of a running download found so far (`None`: not looked for).
    pub(super) fn publisher_lookup_of(&self, id: i64) -> Option<PublisherLookup> {
        self.inner.publisher.running.lock().unwrap().get(&id).cloned()
    }

    pub(super) fn forget_publisher_lookup(&self, id: i64) {
        self.inner.publisher.running.lock().unwrap().remove(&id);
    }

    /// Look for the publisher's checksum of a download that is starting (when it has none of its own and the
    /// setting is on), once its server has answered and the transfer has begun (a server that refuses or cannot be
    /// reached is never asked for more); the engine waits for the answer before it finishes the file.
    pub(super) fn look_for_publisher_checksum(&self, id: i64, rec: &DownloadRecord, control: &DownloadControl, progress: &ProgressHandle) {
        self.forget_publisher_lookup(id);
        let own = rec.checksum.is_some() && rec.checksum_source.is_none();
        let Ok(url) = Url::parse(&rec.url) else { return };
        if own || !self.publisher_check() || candidates(&url, self.inner.cfg.publisher_over_http).is_empty() {
            return;
        }
        let (tx, rx) = watch::channel(PublisherLookup::Looking);
        control.set_publisher(rx);
        let (this, progress) = (self.clone(), progress.clone());
        tokio::spawn(async move {
            // Until the transfer begins; the run ending first (its control, holding the other end, is gone) ends this.
            while progress.transfer_started().is_none() {
                if tx.is_closed() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            this.inner.publisher.running.lock().unwrap().insert(id, PublisherLookup::Looking);
            let found = this.find_publisher_checksum(&url).await;
            let still = match this.inner.publisher.running.lock().unwrap().get_mut(&id) {
                Some(slot) => {
                    *slot = found.clone();
                    true
                }
                None => false,
            };
            let _ = tx.send(found);
            // Its card says at once that the file will be checked.
            if still {
                if let Ok(dto) = this.dto_for(id).await {
                    this.emit(ManagerEvent::Updated { download: dto });
                }
            }
        });
    }

    /// The publisher's checksum for `url`'s file: asked from the lists next to it (or the answer kept from a moment
    /// ago). Never longer than [`LOOKUP_BUDGET`]; anything unexpected is "none".
    pub(super) async fn find_publisher_checksum(&self, url: &Url) -> PublisherLookup {
        let k = key(url);
        if let Some((at, found)) = self.inner.publisher.answers.lock().unwrap().get(&k) {
            if at.elapsed() < KEEP_FOR {
                return found.clone();
            }
        }
        let found = tokio::time::timeout(LOOKUP_BUDGET, self.ask_lists(url)).await.unwrap_or(PublisherLookup::None);
        let mut answers = self.inner.publisher.answers.lock().unwrap();
        answers.retain(|_, (at, _)| at.elapsed() < KEEP_FOR);
        if answers.len() >= KEEP_MAX {
            if let Some(oldest) = answers.iter().min_by_key(|(_, (at, _))| *at).map(|(k, _)| k.clone()) {
                answers.remove(&oldest);
            }
        }
        answers.insert(k, (Instant::now(), found.clone()));
        found
    }

    /// The folder's shared lists first (asked all at once, about one round trip, and kept for every file of the
    /// folder), then the file's own lists (unless the folder's last files had none), the first in order that names the
    /// file. Lookups in the same folder wait for each other, so a batch asks for the shared lists once.
    async fn ask_lists(&self, url: &Url) -> PublisherLookup {
        let Some(file) = link_file_name(url) else { return PublisherLookup::None };
        let lists = candidates(url, self.inner.cfg.publisher_over_http);
        let folder = folder_key(url);
        let lock = {
            let mut asking = self.inner.publisher.asking.lock().unwrap();
            asking.retain(|_, l| Arc::strong_count(l) > 1);
            asking.entry(folder.clone()).or_default().clone()
        };
        let _turn = lock.lock().await;
        let known = {
            let folders = self.inner.publisher.folders.lock().unwrap();
            folders.get(&folder).filter(|f| f.at.elapsed() < KEEP_FOR).map(|f| (f.lists.clone(), f.file_list_misses))
        };
        let (shared, misses) = match known {
            Some(k) => k,
            None => {
                let wanted: Vec<_> = lists.iter().filter(|(_, _, single)| !single).collect();
                let texts = futures_util::future::join_all(wanted.iter().map(|(u, _, _)| self.fetch_list(u))).await;
                let shared: Vec<(String, Option<Arc<String>>)> =
                    wanted.iter().zip(texts).map(|((_, name, _), t)| (name.clone(), t.map(Arc::new))).collect();
                let mut folders = self.inner.publisher.folders.lock().unwrap();
                folders.retain(|_, f| f.at.elapsed() < KEEP_FOR);
                let size = |f: &Folder| f.lists.iter().map(|(_, t)| t.as_ref().map_or(0, |t| t.len())).sum::<usize>();
                let new = Folder { at: Instant::now(), lists: shared.clone(), file_list_misses: 0 };
                while !folders.is_empty()
                    && (folders.len() >= FOLDERS_MAX || folders.values().map(size).sum::<usize>() + size(&new) > FOLDERS_BYTES)
                {
                    let Some(oldest) = folders.iter().min_by_key(|(_, f)| f.at).map(|(k, _)| k.clone()) else { break };
                    folders.remove(&oldest);
                }
                folders.insert(folder.clone(), new);
                (shared, 0)
            }
        };
        let found = |name: &str, checksum| {
            tracing::info!(list = %name, "the publisher's checksum was found next to the file");
            PublisherLookup::Found { checksum, list: name.to_string() }
        };
        for (name, text) in &shared {
            if let Some(checksum) = text.as_deref().and_then(|t| find_in_list(t, &file, false)) {
                return found(name, checksum);
            }
        }
        if misses >= FILE_LIST_MISSES {
            return PublisherLookup::None;
        }
        let own: Vec<_> = lists.iter().filter(|(_, _, single)| *single).collect();
        let texts = futures_util::future::join_all(own.iter().map(|(u, _, _)| self.fetch_list(u))).await;
        let hit =
            own.iter().zip(texts).find_map(|((_, name, _), t)| t.and_then(|t| find_in_list(&t, &file, true)).map(|c| (name.clone(), c)));
        if let Some(f) = self.inner.publisher.folders.lock().unwrap().get_mut(&folder) {
            f.file_list_misses = if hit.is_some() { 0 } else { f.file_list_misses + 1 };
        }
        match hit {
            Some((name, checksum)) => found(&name, checksum),
            None => PublisherLookup::None,
        }
    }

    /// One list, as text: `None` when it is missing, elsewhere, a web page, too large or slow.
    async fn fetch_list(&self, url: &Url) -> Option<String> {
        let read = async {
            // The protected client of every download; `get` sends no cookie, sign-in or custom header.
            let mut resp = self.media_client().get(url).await.ok()?;
            if resp.url().host_str() != url.host_str()
                || resp.url().port_or_known_default() != url.port_or_known_default()
                || resp.url().scheme() != url.scheme()
            {
                return None; // moved to another site: not the publisher's list next to the file
            }
            let html =
                resp.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|t| t.to_ascii_lowercase().contains("html"));
            if html || resp.content_length().is_some_and(|n| n as usize > MAX_LIST) {
                return None;
            }
            let mut out: Vec<u8> = Vec::new();
            while let Some(chunk) = resp.chunk().await.ok()? {
                out.extend_from_slice(&chunk);
                if out.len() > MAX_LIST {
                    return None;
                }
            }
            String::from_utf8(out).ok()
        };
        tokio::time::timeout(LIST_TIMEOUT, read).await.ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_are_looked_for_next_to_the_file_only_over_https() {
        let u = Url::parse("https://cdimage.example.org/12.7.0/amd64/iso-dvd/debian%2012.iso?sig=secret#x").unwrap();
        let c = candidates(&u, false);
        assert_eq!(c[0].0.as_str(), "https://cdimage.example.org/12.7.0/amd64/iso-dvd/debian%2012.iso.sha256");
        assert_eq!(c[0].1, "debian 12.iso.sha256");
        assert!(c[0].2);
        assert!(c.iter().any(|(u, n, single)| n == "SHA256SUMS"
            && !single
            && u.as_str() == "https://cdimage.example.org/12.7.0/amd64/iso-dvd/SHA256SUMS"));
        assert!(
            c.iter().all(|(u, _, _)| u.query().is_none() && u.host_str() == Some("cdimage.example.org")),
            "never the link's signature, never another host"
        );
        assert!(candidates(&Url::parse("http://example.org/f.iso").unwrap(), false).is_empty(), "https only");
        assert!(candidates(&Url::parse("https://example.org/").unwrap(), false).is_empty(), "no file name");
        assert!(candidates(&Url::parse("https://example.org/a/..").unwrap(), false).is_empty());
        assert_eq!(link_file_name(&Url::parse("https://example.org/a/%2E%2E").unwrap()), None);
        assert_eq!(link_file_name(&Url::parse("https://example.org/a/b%2Fc").unwrap()), None);
        assert_eq!(folder_key(&Url::parse("https://example.org/a/b/f.iso?x=1#y").unwrap()), "https://example.org/a/b/");
    }
}
