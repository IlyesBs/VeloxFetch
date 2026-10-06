//! Where downloads go.
//!
//! * The **default folder** can be changed by the user (from Settings). Downloads that already exist stay where
//!   they are; only new ones use the new folder.
//! * A **single download** can be sent to another folder through the confirmation dialog or the Add dialog.
//! * **Sorting by type** (optional): a new file queued for a default folder is put in a sub-folder chosen from
//!   what it is - `Video`, `Audio`, `Images`, `Documents`, `Archives`, `Programs`, `Other` (the names can be changed).
//!   A file the user sent to a folder of their own is put exactly there, and so is one with a sub-folder given.
//!
//! The web view never sends a path. A folder is chosen in a **native dialog** owned by the desktop shell, which
//! hands the answer to [`DownloadManager::register_pick`]; the UI only ever holds the returned token.

use std::collections::{BTreeMap, HashMap};

use downloader_core::{FileKind, Placement};

use super::*;

const ROOT_KEY: &str = "storage.root";
const ALLOWED_KEY: &str = "storage.allowed";
const SORTABLE_KEY: &str = "storage.sortable";
const ORGANIZE_KEY: &str = "organize.enabled";
const NAMES_KEY: &str = "organize.names";
/// Folders remembered (each was picked by the user in a native dialog).
const MAX_FOLDERS: usize = 32;
/// Picks kept in memory for the UI to refer to.
const MAX_PICKS: usize = 64;

pub(super) struct Storage {
    /// Where new downloads go unless told otherwise.
    pub root: PathBuf,
    /// The location the app was configured with (the "reset" target).
    pub app_default: PathBuf,
    /// Every folder that was the default at some time: a queued download is sorted inside the one it was queued for.
    pub sortable: Vec<PathBuf>,
    pub organize: bool,
    /// Folder names the user changed (absent = the default name).
    pub names: BTreeMap<FileKind, String>,
    picks: HashMap<u64, PathBuf>,
    next_pick: u64,
}

fn same_folder(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        let s = s.trim_end_matches('/').to_string();
        if cfg!(any(windows, target_os = "macos")) {
            s.to_lowercase()
        } else {
            s
        }
    };
    norm(a) == norm(b)
}

fn push_unique(list: &mut Vec<PathBuf>, p: PathBuf) {
    if !list.iter().any(|x| same_folder(x, &p)) {
        list.push(p);
    }
}

fn read_list(store: &SqliteStore, key: &str) -> Vec<PathBuf> {
    store
        .get_setting(key)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .take(MAX_FOLDERS)
        .collect()
}

impl Storage {
    pub(super) fn load(store: &SqliteStore, cfg: &ManagerConfig) -> Self {
        let app_default = cfg.download_root.clone();
        let mut root = store
            .get_setting(ROOT_KEY)
            .ok()
            .flatten()
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| app_default.clone());
        // A folder that has gone away (an unplugged drive) must not stop the app: use the default until it is back.
        if std::fs::create_dir_all(&root).is_err() {
            tracing::warn!(folder = %root.display(), "the chosen download folder is not available, using the default one");
            root = app_default.clone();
        }
        let mut sortable = read_list(store, SORTABLE_KEY);
        push_unique(&mut sortable, app_default.clone());
        push_unique(&mut sortable, root.clone());
        let organize = match store.get_setting(ORGANIZE_KEY).ok().flatten().as_deref() {
            Some("true") => true,
            Some("false") => false,
            _ => cfg.organize_default,
        };
        let mut names = BTreeMap::new();
        if let Some(map) =
            store.get_setting(NAMES_KEY).ok().flatten().and_then(|s| serde_json::from_str::<BTreeMap<String, String>>(&s).ok())
        {
            for (k, v) in map {
                if let (Some(kind), Ok(())) = (FileKind::parse(&k), path_safety::check_component(&v)) {
                    names.insert(kind, v);
                }
            }
        }
        Storage { root, app_default, sortable, organize, names, picks: HashMap::new(), next_pick: 0 }
    }

    /// Every folder that may be written to when the app starts: the remembered ones plus the default ones.
    pub(super) fn allowed_at_start(&self, store: &SqliteStore) -> Vec<PathBuf> {
        let mut all = read_list(store, ALLOWED_KEY);
        for p in [self.app_default.clone(), self.root.clone()].into_iter().chain(self.sortable.iter().cloned()) {
            push_unique(&mut all, p);
        }
        all
    }

    pub(super) fn folder_name(&self, kind: FileKind) -> String {
        self.names.get(&kind).cloned().unwrap_or_else(|| kind.default_folder().to_string())
    }

    /// Where a new file called `name` goes when it was queued for `base`.
    pub(super) fn folder_for(&self, base: &Path, name: &str, content_type: Option<&str>) -> PathBuf {
        if self.organize && self.sortable.iter().any(|s| same_folder(s, base)) {
            base.join(self.folder_name(downloader_core::classify_file(name, content_type)))
        } else {
            base.to_path_buf()
        }
    }
}

/// The engine asks this before it creates a file.
pub(super) struct Sorter(pub Arc<Mutex<Storage>>);

impl Placement for Sorter {
    fn folder_for(&self, base: &Path, filename: &str, content_type: Option<&str>) -> PathBuf {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).folder_for(base, filename, content_type)
    }
}

impl DownloadManager {
    fn storage(&self) -> std::sync::MutexGuard<'_, Storage> {
        self.inner.storage.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The folder new downloads go to (unless told otherwise).
    pub fn download_root(&self) -> PathBuf {
        self.storage().root.clone()
    }

    /// The folder a file called `name` of type `content_type` would be saved in if queued now.
    pub(super) fn predicted_dir(&self, name: &str, content_type: Option<&str>) -> PathBuf {
        let s = self.storage();
        s.folder_for(&s.root, name, content_type)
    }

    /// The sub-folder stream downloads go into (`Video`), when sorting is on.
    pub(super) fn sorted_subfolder(&self, kind: FileKind) -> Option<String> {
        let s = self.storage();
        s.organize.then(|| s.folder_name(kind))
    }

    pub fn get_storage(&self) -> StorageDto {
        let s = self.storage();
        StorageDto {
            root: s.root.to_string_lossy().into_owned(),
            default_root: s.app_default.to_string_lossy().into_owned(),
            is_default: same_folder(&s.root, &s.app_default),
            organize: s.organize,
            folders: FileKind::ALL
                .into_iter()
                .map(|kind| FolderNameDto { kind, folder: s.folder_name(kind), default_folder: kind.default_folder().to_string() })
                .collect(),
        }
    }

    async fn put_setting(&self, key: &'static str, value: String) -> Result<(), ManagerError> {
        let store = self.inner.store.clone();
        blocking(move || store.set_setting(key, &value)).await?;
        Ok(())
    }

    async fn persist_folders(&self) -> Result<(), ManagerError> {
        let (root, sortable) = {
            let s = self.storage();
            (s.root.to_string_lossy().into_owned(), s.sortable.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>())
        };
        let allowed: Vec<String> = self.inner.roots.list().iter().map(|p| p.to_string_lossy().into_owned()).collect();
        self.put_setting(ROOT_KEY, root).await?;
        self.put_setting(SORTABLE_KEY, serde_json::to_string(&sortable).unwrap_or_else(|_| "[]".into())).await?;
        self.put_setting(ALLOWED_KEY, serde_json::to_string(&allowed).unwrap_or_else(|_| "[]".into())).await?;
        Ok(())
    }

    /// The desktop shell calls this with the folder the user picked in the native dialog. The folder is
    /// validated (exists, writable, not a system / program / autostart folder, not the app's own data) and
    /// a token is returned; the UI uses the token, never the path.
    pub fn register_pick(&self, path: &Path) -> Result<PickedFolder, ManagerError> {
        let data_dir = self.inner.cfg.db_path.parent().map(Path::to_path_buf).unwrap_or_default();
        let mut protect = vec![data_dir];
        if let Ok(c) = std::fs::canonicalize(&protect[0]) {
            protect.push(path_safety::display_path(&c));
        }
        let folder = path_safety::validate_user_folder(path, &protect).map_err(ManagerError::invalid)?;
        let mut s = self.storage();
        if s.picks.len() >= MAX_PICKS {
            if let Some(oldest) = s.picks.keys().min().copied() {
                s.picks.remove(&oldest);
            }
        }
        s.next_pick += 1;
        let token = s.next_pick;
        s.picks.insert(token, folder.clone());
        Ok(PickedFolder { token, path: folder.to_string_lossy().into_owned() })
    }

    /// The folder behind a token from [`register_pick`](Self::register_pick).
    pub(super) fn pick_path(&self, token: u64) -> Result<PathBuf, ManagerError> {
        self.storage()
            .picks
            .get(&token)
            .cloned()
            .ok_or_else(|| ManagerError::invalid("that folder choice is no longer valid: choose the folder again"))
    }

    /// Remember a picked folder as one downloads may be written to (checked again before every write).
    pub(super) async fn allow_folder(&self, folder: &Path) -> Result<(), ManagerError> {
        let known = self.inner.roots.list().iter().any(|r| same_folder(r, folder));
        if !known {
            if self.inner.roots.list().len() >= MAX_FOLDERS {
                // Forget the oldest one-off folder (never the default ones).
                let keep: Vec<PathBuf> = {
                    let s = self.storage();
                    self.inner.roots.list().into_iter().filter(|r| s.sortable.iter().any(|d| same_folder(d, r))).collect()
                };
                let mut all = self.inner.roots.list();
                if let Some(pos) = all.iter().position(|r| !keep.iter().any(|k| same_folder(k, r))) {
                    all.remove(pos);
                    self.inner.roots.set(all);
                }
            }
            self.inner.roots.add(folder.to_path_buf());
            self.persist_folders().await?;
        }
        Ok(())
    }

    /// Make the picked folder the default for new downloads.
    pub async fn use_folder_as_default(&self, token: u64) -> Result<StorageDto, ManagerError> {
        let folder = self.pick_path(token)?;
        self.allow_folder(&folder).await?;
        {
            let mut s = self.storage();
            s.root = folder.clone();
            push_unique(&mut s.sortable, folder);
            if s.sortable.len() > MAX_FOLDERS {
                s.sortable.remove(0);
            }
        }
        self.persist_folders().await?;
        Ok(self.get_storage())
    }

    /// Go back to the folder the app was installed with.
    pub async fn reset_default_folder(&self) -> Result<StorageDto, ManagerError> {
        let root = self.storage().app_default.clone();
        tokio::fs::create_dir_all(&root).await.map_err(|e| ManagerError::Internal(format!("cannot create the folder: {e}")))?;
        self.storage().root = root;
        self.persist_folders().await?;
        Ok(self.get_storage())
    }

    pub async fn set_organize(&self, on: bool) -> Result<StorageDto, ManagerError> {
        self.storage().organize = on;
        self.put_setting(ORGANIZE_KEY, if on { "true" } else { "false" }.to_string()).await?;
        Ok(self.get_storage())
    }

    /// Rename the type folders. `names` maps a kind (`video`, `audio`, `image`, `document`, `archive`,
    /// `program`, `other`) to a folder name; an empty name means the default one. Names are checked strictly
    /// (one plain name each, no path, no reserved names) and must differ from each other.
    pub async fn set_folder_names(&self, req: SetFolderNames) -> Result<StorageDto, ManagerError> {
        let mut names: BTreeMap<FileKind, String> = BTreeMap::new();
        for (key, value) in &req.names {
            let kind = FileKind::parse(key)
                .ok_or_else(|| ManagerError::invalid(format!("unknown kind `{}`", key.chars().take(20).collect::<String>())))?;
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            validate::check_component(value, "folder name")?;
            names.insert(kind, value.to_string());
        }
        // No two kinds may share a folder.
        let mut seen: Vec<String> = Vec::new();
        for kind in FileKind::ALL {
            let n = names.get(&kind).cloned().unwrap_or_else(|| kind.default_folder().to_string()).to_lowercase();
            if seen.contains(&n) {
                return Err(ManagerError::invalid(format!("two kinds of file would share the folder `{n}`")));
            }
            seen.push(n);
        }
        let json: BTreeMap<&str, &String> = names.iter().map(|(k, v)| (k.key(), v)).collect();
        self.put_setting(NAMES_KEY, serde_json::to_string(&json).unwrap_or_else(|_| "{}".into())).await?;
        self.storage().names = names;
        Ok(self.get_storage())
    }
}
