//! The log: one line per event, secrets removed, kept in a ring in memory (for the UI) and in a small set of
//! rotating files on the user's own disk. Nothing is sent anywhere.
//!
//! A hand-written [`tracing::Subscriber`] (no extra dependency):
//! * **levels**: warnings and errors always; `info` for VeloxFetch's own crates; `debug` only when the user
//!   turned on detailed logging. Other libraries (HTTP, TLS, the web view) are kept to warnings and errors.
//! * **context**: the fields of the spans an event happens in come first (`download{id=12}: segment failed...`),
//!   so every line of a download carries its id.
//! * **secrets**: a field whose *name* is secret (`authorization`, `cookie`, `password`, `token`...) is written as
//!   `[redacted]` whatever its value; every line then goes through [`Redactor`].
//! * **files**: `velox.log`, rotated to `velox.1.log` ... `velox.N.log` at a size limit; created readable by the
//!   user only on Unix (the application data folder already is on Windows and macOS).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata, Subscriber};

use crate::redact::{is_secret_name, Redactor, MASK};

/// Targets written at `info` / `debug`; everything else only from `warn` up.
const OWN_TARGETS: &[&str] = &[
    "velox",
    "download_manager",
    "downloader_core",
    "http_client",
    "persistence",
    "browser_bridge",
    "media_engine",
    "scheduler",
    "system_integration",
    "path_safety",
    "secrets",
    "diagnostics",
    "native_host",
];

#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Folder of the log files; `None` keeps the log in memory only.
    pub dir: Option<PathBuf>,
    /// Name of the current file (`velox.log`); older ones get `.1`, `.2`... before the extension.
    pub file_name: String,
    /// The current file is rotated once it would grow past this.
    pub max_file_bytes: u64,
    /// Rotated files kept besides the current one.
    pub keep_files: usize,
    /// Lines kept in memory for the UI.
    pub ring_lines: usize,
    /// Replaced by `~` in every line (the user's home folder).
    pub home: Option<PathBuf>,
    /// Also print every line on standard error (the command-line tool).
    pub stderr: bool,
    /// First line of every session, e.g. `VeloxFetch 0.1.0`.
    pub app: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            dir: None,
            file_name: "velox.log".into(),
            max_file_bytes: 2 * 1024 * 1024,
            keep_files: 3,
            ring_lines: 1000,
            home: None,
            stderr: false,
            app: "VeloxFetch".into(),
        }
    }
}

/// One line of the log, already redacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// Unix milliseconds.
    pub at: i64,
    /// `ERROR`, `WARN`, `INFO`, `DEBUG` or `TRACE`.
    pub level: &'static str,
    pub target: String,
    /// Span context, message and fields.
    pub text: String,
}

impl LogLine {
    /// As written to the file.
    pub fn to_file_line(&self) -> String {
        format!("{} {:<5} {}: {}", crate::utc_timestamp(self.at, true), self.level, self.target, self.text)
    }

    pub fn is_problem(&self) -> bool {
        matches!(self.level, "ERROR" | "WARN")
    }
}

struct Span {
    meta: &'static Metadata<'static>,
    name: &'static str,
    fields: String,
    refs: usize,
}

struct Shared {
    cfg: LogConfig,
    redactor: Redactor,
    detailed: AtomicBool,
    ring: Mutex<VecDeque<LogLine>>,
    file: Mutex<Option<RotatingFile>>,
    spans: Mutex<HashMap<u64, Span>>,
    next_span: AtomicU64,
}

/// Cheap to clone; reads the recent lines, switches detailed logging, tells where the files are.
#[derive(Clone)]
pub struct LogHandle(Arc<Shared>);

impl std::fmt::Debug for LogHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogHandle").field("dir", &self.0.cfg.dir).field("detailed", &self.detailed()).finish()
    }
}

thread_local! {
    /// Spans entered on this thread, as (subscriber, span id).
    static STACK: RefCell<Vec<(usize, u64)>> = const { RefCell::new(Vec::new()) };
    /// Set while this thread formats a line, so a `Debug` impl that logs cannot re-enter.
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

impl LogHandle {
    /// A log with nothing installed yet: use [`dispatch`](Self::dispatch) (tests) or [`install`](Self::install).
    /// If the folder cannot be used, the log stays in memory only.
    pub fn new(cfg: LogConfig) -> Self {
        let file = cfg.dir.as_deref().and_then(|dir| RotatingFile::open(dir, &cfg.file_name).ok());
        let redactor = Redactor::with_home(cfg.home.as_deref());
        let handle = Self(Arc::new(Shared {
            redactor,
            detailed: AtomicBool::new(false),
            ring: Mutex::new(VecDeque::with_capacity(cfg.ring_lines.min(4096))),
            file: Mutex::new(file),
            spans: Mutex::new(HashMap::new()),
            next_span: AtomicU64::new(1),
            cfg,
        }));
        let os = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
        handle.write("INFO", "velox", &format!("{} started on {os}", handle.0.cfg.app));
        handle
    }

    /// Make this the process-wide log and record panics in it. Returns false (and changes nothing) when
    /// another logger was installed first.
    pub fn install(&self) -> bool {
        if tracing::dispatcher::set_global_default(self.dispatch()).is_err() {
            return false;
        }
        let handle = self.clone();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let what = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown cause".into());
            let at = info.location().map_or_else(String::new, |l| format!(" at {}:{}", l.file(), l.line()));
            let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
            let trace = std::backtrace::Backtrace::force_capture().to_string();
            let trace: String = trace.lines().take(60).collect::<Vec<_>>().join("\n    ");
            handle.write_panic(&format!("crash (panic) in thread '{thread}'{at}: {what}\n    {trace}"));
            previous(info);
        }));
        true
    }

    /// A subscriber writing into this log (for `tracing::dispatcher::with_default` in tests).
    pub fn dispatch(&self) -> tracing::Dispatch {
        tracing::Dispatch::new(LogSubscriber(self.0.clone()))
    }

    /// Detailed logging adds the engine's `debug` lines (connection changes, fallbacks...).
    pub fn set_detailed(&self, on: bool) {
        if self.0.detailed.swap(on, Ordering::Relaxed) != on {
            self.write("INFO", "velox", if on { "detailed logging turned on" } else { "detailed logging turned off" });
        }
    }

    pub fn detailed(&self) -> bool {
        self.0.detailed.load(Ordering::Relaxed)
    }

    /// The newest `limit` lines, oldest first.
    pub fn recent(&self, limit: usize) -> Vec<LogLine> {
        let ring = self.0.ring.lock().unwrap_or_else(|p| p.into_inner());
        ring.iter().skip(ring.len().saturating_sub(limit)).cloned().collect()
    }

    /// The folder of the log files, if the log is written to disk.
    pub fn dir(&self) -> Option<&Path> {
        self.0.cfg.dir.as_deref().filter(|_| self.writes_files())
    }

    /// The current log file, if any.
    pub fn file(&self) -> Option<PathBuf> {
        self.dir().map(|d| d.join(&self.0.cfg.file_name))
    }

    pub fn writes_files(&self) -> bool {
        self.0.file.lock().unwrap_or_else(|p| p.into_inner()).is_some()
    }

    /// The redactor every line goes through (for reports built outside the log).
    pub fn redactor(&self) -> &Redactor {
        &self.0.redactor
    }

    /// Add a line directly (redacted like any other).
    pub fn write(&self, level: &'static str, target: &str, text: &str) {
        let line = LogLine { at: crate::now_ms(), level, target: target.to_string(), text: self.0.redactor.redact(text).into_owned() };
        self.0.keep(line, true);
    }

    /// From the panic hook: never waits for a lock the panicking thread may hold.
    fn write_panic(&self, text: &str) {
        let line =
            LogLine { at: crate::now_ms(), level: "ERROR", target: "velox::crash".into(), text: self.0.redactor.redact(text).into_owned() };
        self.0.keep(line, false);
    }
}

impl Shared {
    fn keep(&self, line: LogLine, wait: bool) {
        let file_line = line.to_file_line();
        if self.cfg.stderr {
            eprintln!("{file_line}");
        }
        let file = if wait { Some(self.file.lock().unwrap_or_else(|p| p.into_inner())) } else { self.file.try_lock().ok() };
        if let Some(mut file) = file {
            if let Some(f) = file.as_mut() {
                f.write_line(&file_line, self.cfg.max_file_bytes, self.cfg.keep_files);
            }
        }
        let ring = if wait { Some(self.ring.lock().unwrap_or_else(|p| p.into_inner())) } else { self.ring.try_lock().ok() };
        if let Some(mut ring) = ring {
            while ring.len() >= self.cfg.ring_lines.max(1) {
                ring.pop_front();
            }
            ring.push_back(line);
        }
    }

    fn level_allowed(&self, meta: &Metadata<'_>) -> bool {
        let max = match (is_own(meta.target()), self.detailed.load(Ordering::Relaxed)) {
            (false, _) => Level::WARN,
            (true, false) => Level::INFO,
            (true, true) => Level::DEBUG,
        };
        *meta.level() <= max
    }
}

/// Crate names are prefixes of their module paths (`downloader_core::engine`, `velox_desktop_lib`).
fn is_own(target: &str) -> bool {
    OWN_TARGETS.iter().any(|t| target.starts_with(t))
}

fn level_name(l: &Level) -> &'static str {
    match *l {
        Level::ERROR => "ERROR",
        Level::WARN => "WARN",
        Level::INFO => "INFO",
        Level::DEBUG => "DEBUG",
        Level::TRACE => "TRACE",
    }
}

/// Collects `message` and the other fields of an event or span.
#[derive(Default)]
struct Fields {
    message: String,
    rest: String,
}

impl Fields {
    fn push(&mut self, name: &str, value: std::fmt::Arguments<'_>, quote_spaces: bool) {
        if name == "message" {
            let _ = self.message.write_fmt(value);
            return;
        }
        if !self.rest.is_empty() {
            self.rest.push(' ');
        }
        if is_secret_name(name) {
            let _ = write!(self.rest, "{name}={MASK}");
            return;
        }
        let v = value.to_string();
        if quote_spaces && v.contains(char::is_whitespace) {
            let _ = write!(self.rest, "{name}={v:?}");
        } else {
            let _ = write!(self.rest, "{name}={v}");
        }
    }
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.push(field.name(), format_args!("{value:?}"), false);
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field.name(), format_args!("{value}"), true);
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.push(field.name(), format_args!("{value}"), false);
    }
}

struct LogSubscriber(Arc<Shared>);

impl LogSubscriber {
    fn key(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }
}

impl Subscriber for LogSubscriber {
    fn register_callsite(&self, meta: &'static Metadata<'static>) -> Interest {
        if *meta.level() <= Level::WARN {
            Interest::always()
        } else if *meta.level() == Level::TRACE || !is_own(meta.target()) {
            // Libraries' chatter (hyper, h2, rustls log on every frame) is never wanted: decided once, for free.
            Interest::never()
        } else {
            // Our info / debug lines: asked every time, detailed logging can be switched while running.
            Interest::sometimes()
        }
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::DEBUG)
    }

    fn enabled(&self, meta: &Metadata<'_>) -> bool {
        self.0.level_allowed(meta)
    }

    fn new_span(&self, attrs: &Attributes<'_>) -> Id {
        let mut f = Fields::default();
        attrs.record(&mut f);
        let id = self.0.next_span.fetch_add(1, Ordering::Relaxed).max(1);
        let fields = if f.message.is_empty() { f.rest } else { format!("{} {}", f.message, f.rest).trim().to_string() };
        self.0
            .spans
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id, Span { meta: attrs.metadata(), name: attrs.metadata().name(), fields, refs: 1 });
        Id::from_u64(id)
    }

    fn record(&self, span: &Id, values: &Record<'_>) {
        let mut f = Fields::default();
        values.record(&mut f);
        if let Some(s) = self.0.spans.lock().unwrap_or_else(|p| p.into_inner()).get_mut(&span.into_u64()) {
            if !s.fields.is_empty() && !f.rest.is_empty() {
                s.fields.push(' ');
            }
            s.fields.push_str(&f.rest);
        }
    }

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        if BUSY.with(|b| b.replace(true)) {
            return;
        }
        let meta = event.metadata();
        let mut f = Fields::default();
        event.record(&mut f);
        let mut text = String::new();
        let stack: Vec<u64> = STACK.with(|s| s.borrow().iter().filter(|(k, _)| *k == self.key()).map(|(_, id)| *id).collect());
        if !stack.is_empty() {
            let spans = self.0.spans.lock().unwrap_or_else(|p| p.into_inner());
            for id in stack {
                if let Some(s) = spans.get(&id) {
                    let _ = write!(text, "{}{{{}}}: ", s.name, s.fields);
                }
            }
        }
        text.push_str(&f.message);
        if !f.rest.is_empty() {
            if !f.message.is_empty() {
                text.push(' ');
            }
            text.push_str(&f.rest);
        }
        let line = LogLine {
            at: crate::now_ms(),
            level: level_name(meta.level()),
            target: meta.target().to_string(),
            text: self.0.redactor.redact(&text).into_owned(),
        };
        self.0.keep(line, true);
        BUSY.with(|b| b.set(false));
    }

    /// The innermost span entered on this thread (so `Span::current()` works, e.g. to carry a download's
    /// span into the tasks it spawns).
    fn current_span(&self) -> tracing_core::span::Current {
        let top = STACK.with(|s| s.borrow().iter().rev().find(|(k, _)| *k == self.key()).map(|(_, id)| *id));
        let meta = top.and_then(|id| self.0.spans.lock().unwrap_or_else(|p| p.into_inner()).get(&id).map(|s| (id, s.meta)));
        match meta {
            Some((id, meta)) => tracing_core::span::Current::new(Id::from_u64(id), meta),
            None => tracing_core::span::Current::none(),
        }
    }

    fn enter(&self, span: &Id) {
        STACK.with(|s| s.borrow_mut().push((self.key(), span.into_u64())));
    }

    fn exit(&self, span: &Id) {
        STACK.with(|s| {
            let mut s = s.borrow_mut();
            if let Some(pos) = s.iter().rposition(|e| *e == (self.key(), span.into_u64())) {
                s.remove(pos);
            }
        });
    }

    fn clone_span(&self, id: &Id) -> Id {
        if let Some(s) = self.0.spans.lock().unwrap_or_else(|p| p.into_inner()).get_mut(&id.into_u64()) {
            s.refs += 1;
        }
        id.clone()
    }

    fn try_close(&self, id: Id) -> bool {
        let mut spans = self.0.spans.lock().unwrap_or_else(|p| p.into_inner());
        let key = id.into_u64();
        match spans.get_mut(&key) {
            Some(s) if s.refs > 1 => {
                s.refs -= 1;
                false
            }
            Some(_) => {
                spans.remove(&key);
                true
            }
            None => false,
        }
    }
}

struct RotatingFile {
    dir: PathBuf,
    name: String,
    /// `None` only if the file could not be reopened after a rotation (lines then go to the ring only).
    file: Option<File>,
    size: u64,
}

impl RotatingFile {
    fn open(dir: &Path, name: &str) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let file = open_private(&dir.join(name))?;
        let size = file.metadata().map_or(0, |m| m.len());
        Ok(Self { dir: dir.to_path_buf(), name: name.to_string(), file: Some(file), size })
    }

    /// `velox.log` -> `velox.{n}.log`.
    fn numbered(&self, n: usize) -> PathBuf {
        let name = match self.name.rsplit_once('.') {
            Some((stem, ext)) => format!("{stem}.{n}.{ext}"),
            None => format!("{}.{n}", self.name),
        };
        self.dir.join(name)
    }

    fn write_line(&mut self, line: &str, max: u64, keep: usize) {
        let len = line.len() as u64 + 1;
        if self.size > 0 && self.size + len > max {
            self.rotate(keep);
        }
        if let Some(f) = self.file.as_mut() {
            if writeln!(f, "{line}").is_ok() {
                self.size += len;
            }
        }
    }

    fn rotate(&mut self, keep: usize) {
        let current = self.dir.join(&self.name);
        // Windows cannot rename or delete a file that is open without sharing: close ours first.
        self.file = None;
        if keep == 0 {
            let _ = std::fs::remove_file(&current);
        } else {
            let _ = std::fs::remove_file(self.numbered(keep));
            for n in (1..keep).rev() {
                let _ = std::fs::rename(self.numbered(n), self.numbered(n + 1));
            }
            let _ = std::fs::rename(&current, self.numbered(1));
        }
        self.file = open_private(&current).ok();
        self.size = self.file.as_ref().and_then(|f| f.metadata().ok()).map_or(0, |m| m.len());
    }
}

fn open_private(path: &Path) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("velox-log-{tag}-{}-{}", std::process::id(), crate::now_ms()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn texts(h: &LogHandle) -> Vec<String> {
        h.recent(1000).into_iter().map(|l| l.text).collect()
    }

    #[test]
    fn secret_fields_and_values_never_reach_the_ring_or_the_file() {
        let dir = temp_dir("secrets");
        let h = LogHandle::new(LogConfig { dir: Some(dir.clone()), ..Default::default() });
        tracing::dispatcher::with_default(&h.dispatch(), || {
            let headers =
                vec![("authorization".to_string(), "Basic dXNlcjpodW50ZXIy".to_string()), ("cookie".into(), "sid=COOKIEVAL".into())];
            tracing::info!(authorization = "Bearer TOPSECRET1", password = "hunter2", ?headers, "request");
            tracing::warn!(error = "error sending request for url (https://bob:pw9@files.test/a.zip?token=QUERYTOK)", "failed");
            tracing::info!("proxy password={} cookie: {}", "PROXYPW", "a=COOKIE2");
        });
        let file = std::fs::read_to_string(dir.join("velox.log")).unwrap();
        let ring = texts(&h).join("\n");
        for secret in ["TOPSECRET1", "hunter2", "dXNlcjpodW50ZXIy", "COOKIEVAL", "pw9", "QUERYTOK", "PROXYPW", "COOKIE2", "bob:"] {
            assert!(!file.contains(secret), "{secret} in file:\n{file}");
            assert!(!ring.contains(secret), "{secret} in ring:\n{ring}");
        }
        assert!(file.contains("request authorization=[redacted] password=[redacted]"), "{file}");
        assert!(file.contains("files.test/a.zip?token=[redacted]"), "{file}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn levels_detailed_switch_and_foreign_targets() {
        let h = LogHandle::new(LogConfig::default());
        tracing::dispatcher::with_default(&h.dispatch(), || {
            tracing::debug!("hidden debug");
            tracing::info!(target: "hyper::client", "noisy library info");
            tracing::warn!(target: "hyper::client", "library warning");
            h.set_detailed(true);
            tracing::debug!("visible debug");
            h.set_detailed(false);
            tracing::debug!("hidden again");
        });
        let t = texts(&h);
        assert!(t.iter().any(|l| l == "visible debug"), "{t:?}");
        assert!(t.iter().any(|l| l == "library warning"));
        assert!(!t.iter().any(|l| l.contains("hidden") || l.contains("noisy")), "{t:?}");
    }

    #[test]
    fn span_fields_prefix_each_line() {
        let h = LogHandle::new(LogConfig::default());
        tracing::dispatcher::with_default(&h.dispatch(), || {
            let span = tracing::info_span!("download", id = 42);
            let _g = span.enter();
            tracing::warn!(segment = 3, "segment failed, retrying");
        });
        assert!(texts(&h).iter().any(|l| l == "download{id=42}: segment failed, retrying segment=3"), "{:?}", texts(&h));
    }

    #[test]
    fn files_rotate_and_old_ones_are_dropped() {
        let dir = temp_dir("rotate");
        let h = LogHandle::new(LogConfig { dir: Some(dir.clone()), max_file_bytes: 2_000, keep_files: 2, ..Default::default() });
        for i in 0..200 {
            h.write("INFO", "velox", &format!("line {i} {}", "x".repeat(40)));
        }
        let mut names: Vec<String> =
            std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["velox.1.log", "velox.2.log", "velox.log"]);
        for n in &names {
            assert!(std::fs::metadata(dir.join(n)).unwrap().len() <= 2_000, "{n}");
        }
        assert!(std::fs::read_to_string(dir.join("velox.log")).unwrap().contains("line 199"));
        assert_eq!(h.recent(5).len(), 5);
        assert!(h.recent(5).last().unwrap().text.starts_with("line 199"));
        drop(h);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_ring_is_bounded_and_a_missing_folder_means_memory_only() {
        let blocker = temp_dir("blocked");
        std::fs::write(&blocker, b"a file, not a folder").unwrap();
        let h = LogHandle::new(LogConfig { dir: Some(blocker.join("logs")), ring_lines: 10, ..Default::default() });
        assert!(!h.writes_files());
        assert!(h.dir().is_none());
        for i in 0..50 {
            h.write("WARN", "velox", &format!("n{i}"));
        }
        let t = texts(&h);
        assert_eq!(t.len(), 10);
        assert_eq!(t.last().unwrap(), "n49");
        let _ = std::fs::remove_file(blocker);
    }

    /// The only test that installs the process-wide log (and its crash hook) in this test binary.
    #[test]
    fn install_records_crashes_without_their_secrets() {
        let dir = temp_dir("panic");
        let h = LogHandle::new(LogConfig { dir: Some(dir.clone()), ..Default::default() });
        assert!(h.install());
        assert!(!LogHandle::new(LogConfig::default()).install(), "only one process-wide log");
        let crashed = std::thread::Builder::new().name("worker".into()).spawn(|| panic!("boom password=hunter2")).unwrap().join();
        assert!(crashed.is_err());
        let text = std::fs::read_to_string(dir.join("velox.log")).unwrap();
        assert!(text.contains("ERROR velox::crash: crash (panic) in thread 'worker' at "), "{text}");
        assert!(text.contains("boom password=[redacted]") && !text.contains("hunter2"), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("perm");
        let h = LogHandle::new(LogConfig { dir: Some(dir.clone()), ..Default::default() });
        let mode = std::fs::metadata(h.file().unwrap()).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
