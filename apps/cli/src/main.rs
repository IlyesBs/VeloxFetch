//! `velox` — command-line front end for the VeloxFetch download engine.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};
use downloader_core::{
    BasicAuth, Checksum, ClientConfig, DownloadConfig, DownloadControl, DownloadError, DownloadRequest, DownloadStore, Downloader,
    Priority, ProgressHandle,
};
use persistence::{RecoveryAction, SqliteStore};

#[derive(Parser)]
#[command(name = "velox", version, about = "VeloxFetch download engine CLI")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct Net {
    /// Extra request header, `Name: value` (repeatable)
    #[arg(short = 'H', long = "header")]
    headers: Vec<String>,
    #[arg(long)]
    user_agent: Option<String>,
    /// Proxy URL (http/https/socks5). Prefer the VELOX_PROXY environment variable when it contains a
    /// password: command lines are visible to other users and stay in shell history.
    #[arg(long)]
    proxy: Option<String>,
    /// HTTP basic auth as `user:password`. Prefer the VELOX_AUTH environment variable: command lines
    /// are visible to other users and stay in shell history.
    #[arg(long)]
    auth: Option<String>,
    #[arg(long, default_value_t = 15)]
    connect_timeout: u64,
    #[arg(long, default_value_t = 30)]
    request_timeout: u64,
    #[arg(long, default_value_t = 30)]
    read_timeout: u64,
}

#[derive(Args, Clone)]
struct Tuning {
    /// Maximum parallel connections
    #[arg(short = 'c', long, default_value_t = 16)]
    connections: usize,
    /// Start at the size-based connection count instead of ramping up adaptively
    #[arg(long)]
    fixed_connections: bool,
    /// Maximum automatic retries per segment
    #[arg(long, default_value_t = 5)]
    retries: u32,
    /// Speed limit, e.g. `500K`, `2M`, `1.5M` (bytes per second)
    #[arg(long)]
    limit: Option<String>,
}

#[derive(Args, Clone)]
struct Db {
    /// SQLite state file: enables pause/resume and crash recovery
    #[arg(long, env = "VELOX_DB")]
    db: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show what the server reports about a URL (size, range support, validators)
    Info {
        url: String,
        #[command(flatten)]
        net: Net,
    },
    /// Download a URL
    Get {
        url: String,
        /// Destination directory
        #[arg(short = 'd', long, default_value = ".")]
        dir: PathBuf,
        /// Output file name (sanitized)
        #[arg(short = 'o', long)]
        output: Option<String>,
        #[arg(long)]
        overwrite: bool,
        /// lowest | low | normal | high | highest (recorded; used when a scheduler is running)
        #[arg(long, default_value = "normal")]
        priority: String,
        /// Bandwidth category (shares a limit with other downloads in the same category)
        #[arg(long)]
        category: Option<String>,
        /// Verify the result, e.g. `sha256:<hex>` (take it from a source you trust, not the download server)
        #[arg(long)]
        checksum: Option<String>,
        #[command(flatten)]
        tune: Tuning,
        #[command(flatten)]
        db: Db,
        #[command(flatten)]
        net: Net,
    },
    /// Resume a paused / interrupted download from the state database
    Resume {
        id: i64,
        #[command(flatten)]
        tune: Tuning,
        #[command(flatten)]
        db: Db,
        #[command(flatten)]
        net: Net,
    },
    /// Cancel a paused/interrupted download: deletes its partial data for good
    Cancel {
        id: i64,
        #[command(flatten)]
        db: Db,
    },
    /// List downloads in the state database
    List {
        #[command(flatten)]
        db: Db,
    },
}

fn client_cfg(n: &Net) -> Result<ClientConfig, String> {
    let mut c = ClientConfig::default();
    for h in &n.headers {
        let (k, v) = h.split_once(':').ok_or_else(|| format!("bad header `{h}` (expected `Name: value`)"))?;
        c.headers.push((k.trim().to_string(), v.trim().to_string()));
    }
    if let Some(ua) = &n.user_agent {
        c.user_agent = ua.clone();
    }
    if n.auth.is_some() || n.proxy.as_deref().is_some_and(|p| p.contains('@')) {
        eprintln!("warning: a password on the command line can be seen by other users and is kept in shell history; use VELOX_AUTH / VELOX_PROXY instead");
    }
    let from_env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    c.proxy = n.proxy.clone().or_else(|| from_env("VELOX_PROXY"));
    if let Some(a) = n.auth.clone().or_else(|| from_env("VELOX_AUTH")).as_ref() {
        let (u, p) = a.split_once(':').map_or((a.as_str(), None), |(u, p)| (u, Some(p.to_string())));
        c.basic_auth = Some(BasicAuth { username: u.to_string(), password: p });
    }
    c.connect_timeout = Duration::from_secs(n.connect_timeout);
    c.request_timeout = Duration::from_secs(n.request_timeout);
    c.read_timeout = Duration::from_secs(n.read_timeout);
    Ok(c)
}

fn human(b: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", U[i])
}

#[tokio::main]
async fn main() {
    path_safety::harden_process();
    // `VELOX_LOG=info` (or `debug`) prints the engine's log on standard error, with secrets removed.
    if let Some(level) = std::env::var("VELOX_LOG").ok().filter(|v| !v.is_empty() && v != "0") {
        let log = diagnostics::LogHandle::new(diagnostics::LogConfig {
            stderr: true,
            ring_lines: 1,
            app: format!("velox {}", env!("CARGO_PKG_VERSION")),
            ..Default::default()
        });
        log.set_detailed(level.eq_ignore_ascii_case("debug"));
        log.install();
    }
    if let Err(e) = run().await {
        eprintln!("error: {}", diagnostics::redact(&e));
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    match Cli::parse().cmd {
        Cmd::Info { url, net } => {
            let d = Downloader::new(&client_cfg(&net)?, DownloadConfig::default()).map_err(|e| e.to_string())?;
            let i = d.probe(&url).await.map_err(|e| e.to_string())?;
            println!("url            : {}", i.final_url);
            println!("size           : {}", i.content_length.map_or("unknown".into(), |s| format!("{s} ({})", human(s))));
            println!("type           : {}", i.content_type.as_deref().unwrap_or("-"));
            println!("disposition    : {}", i.content_disposition.as_deref().unwrap_or("-"));
            println!("accept-ranges  : {} (claimed)", i.accept_ranges_header);
            println!("range verified : {}", i.range_verified);
            println!("etag           : {}", i.etag.as_deref().unwrap_or("-"));
            println!("last-modified  : {}", i.last_modified.as_deref().unwrap_or("-"));
            println!("mode           : {}", if i.supports_parallel() { "parallel" } else { "sequential" });
        }
        Cmd::Get { url, dir, output, overwrite, priority, category, checksum, tune, db, net } => {
            let mut req = DownloadRequest::new(url, dir);
            req.filename = output;
            req.overwrite = overwrite;
            req.priority = Priority::parse(&priority).ok_or("priority must be lowest|low|normal|high|highest")?;
            req.category = category;
            if let Some(c) = checksum {
                req.expected_checksum = Some(Checksum::parse(&c)?);
            }
            run_download(req, tune, db, net).await?;
        }
        Cmd::Resume { id, tune, db, net } => {
            if db.db.is_none() {
                return Err("resume needs --db (or VELOX_DB)".into());
            }
            run_download(DownloadRequest::resume(id), tune, db, net).await?;
        }
        Cmd::Cancel { id, db } => {
            let store = open_store(&db)?.ok_or("cancel needs --db (or VELOX_DB)")?;
            let d = Downloader::new(&ClientConfig::default(), DownloadConfig::default())
                .map_err(|e| e.to_string())?
                .with_store(store as Arc<dyn DownloadStore>);
            d.cancel(id).await.map_err(|e| e.to_string())?;
            println!("cancelled #{id}; partial data deleted");
        }
        Cmd::List { db } => {
            let store = open_store(&db)?.ok_or("list needs --db (or VELOX_DB)")?;
            for r in store.list().map_err(|e| e.to_string())? {
                let pct =
                    r.total_size.filter(|t| *t > 0).map_or(String::new(), |t| format!(" {:.1}%", r.downloaded as f64 * 100.0 / t as f64));
                println!(
                    "#{:<4} {:<11}{} {}  ({})",
                    r.id.unwrap_or_default(),
                    r.status.as_str(),
                    pct,
                    r.filename,
                    r.last_error.as_deref().unwrap_or("-")
                );
            }
        }
    }
    Ok(())
}

/// Open the state DB (if requested) and run crash recovery before anything else touches it.
fn open_store(db: &Db) -> Result<Option<Arc<SqliteStore>>, String> {
    let Some(path) = &db.db else { return Ok(None) };
    let store = Arc::new(SqliteStore::open(path).map_err(|e| e.to_string())?);
    for r in store.recover_interrupted().map_err(|e| e.to_string())? {
        match r.action {
            RecoveryAction::ResumeReady { downloaded } => {
                eprintln!("recovered #{} {}: {} saved, ready to resume", r.id, r.filename, human(downloaded))
            }
            RecoveryAction::Restart { reason } => {
                eprintln!("recovered #{} {}: will restart ({reason})", r.id, r.filename)
            }
            RecoveryAction::AlreadyFinished => eprintln!("recovered #{} {}: already finished", r.id, r.filename),
        }
    }
    Ok(Some(store))
}

/// `500K`, `2M`, `1.5G`, or plain bytes -> bytes per second.
fn parse_rate(s: &str) -> Result<u64, String> {
    let t = s.trim().to_ascii_uppercase();
    let (num, mult) = match t.chars().last() {
        Some('K') => (&t[..t.len() - 1], 1024.0),
        Some('M') => (&t[..t.len() - 1], 1024.0 * 1024.0),
        Some('G') => (&t[..t.len() - 1], 1024.0 * 1024.0 * 1024.0),
        _ => (t.as_str(), 1.0),
    };
    let v: f64 = num.trim().parse().map_err(|_| format!("bad rate `{s}` (try 500K, 2M)"))?;
    if v <= 0.0 {
        return Err("rate must be positive".into());
    }
    Ok((v * mult) as u64)
}

async fn run_download(mut req: DownloadRequest, tune: Tuning, db: Db, net: Net) -> Result<(), String> {
    let mut cfg = DownloadConfig { max_connections: tune.connections, ..Default::default() };
    cfg.retry.max_retries = tune.retries;
    cfg.adaptive.enabled = !tune.fixed_connections;
    if let Some(l) = &tune.limit {
        req.speed_limit = Some(parse_rate(l)?);
    }
    let mut d = Downloader::new(&client_cfg(&net)?, cfg).map_err(|e| e.to_string())?;
    let stored = open_store(&db)?;
    if let Some(s) = &stored {
        d = d.with_store(s.clone() as Arc<dyn DownloadStore>);
    }
    let control = DownloadControl::new();
    let progress = ProgressHandle::new();

    // Ctrl-C once = pause (keeps progress). Ctrl-C again = cancel (discards partial data).
    let c = control.clone();
    let keeps_state = stored.is_some();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\n{}", if keeps_state { "pausing… (Ctrl-C again to cancel and discard)" } else { "stopping…" });
            c.pause();
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("\ncancelling…");
                c.cancel();
            }
        }
    });

    let p = progress.clone();
    let ticker = tokio::spawn(async move {
        let start = Instant::now();
        let mut announced = false;
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if !announced {
                if let Some(id) = p.download_id() {
                    eprintln!("download id {id}");
                    announced = true;
                }
            }
            let s = p.snapshot();
            let rate = s.downloaded as f64 / start.elapsed().as_secs_f64().max(0.001);
            let retries: u32 = s.segments.iter().map(|g| g.retry_count).sum();
            let r = if retries > 0 { format!("  retries: {retries}") } else { String::new() };
            match s.total {
                Some(t) => eprint!(
                    "\r{:>5.1}%  {} / {}  {}/s{r}   ",
                    s.downloaded as f64 * 100.0 / t as f64,
                    human(s.downloaded),
                    human(t),
                    human(rate as u64)
                ),
                None => eprint!("\r{}  {}/s{r}   ", human(s.downloaded), human(rate as u64)),
            }
        }
    });

    let started = Instant::now();
    let res = d.download(&req, control, progress.clone()).await;
    ticker.abort();
    eprintln!();
    match res {
        Ok(o) => {
            println!(
                "saved {} ({}) in {:.1}s using {} connection(s), {}{}{}",
                o.path.display(),
                human(o.size),
                started.elapsed().as_secs_f64(),
                o.connections,
                if o.parallel { "parallel" } else { "sequential" },
                if o.resumed { ", resumed" } else { "" },
                if o.checksum_verified { ", checksum OK" } else { "" },
            );
            Ok(())
        }
        Err(DownloadError::Paused) => {
            match (progress.download_id(), stored.is_some()) {
                (Some(id), true) => println!("paused. resume with: velox resume {id} --db <state.db>"),
                _ => println!("stopped (no --db, so progress was not saved and cannot be resumed)"),
            }
            Ok(())
        }
        Err(DownloadError::Cancelled) => {
            println!("cancelled; partial data deleted");
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}
