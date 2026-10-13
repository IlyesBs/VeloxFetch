//! `velox-bench`: the Phase 20 performance benchmark of the download engine (not shipped).
//!
//! ```text
//! velox-bench run  [--sizes 100M,1G] [--conns 1,4,8,16,32,64] [--adaptive] [--per-conn 0] [--repeat 1] [--buffer-kib 64]
//!                  [--dir <folder>] [--json <file>]
//! velox-bench db   [--segments 64] [--saves 2000]
//! velox-bench serve                      (started by `run`: the local test server, in its own process)
//! velox-bench case ...                   (started by `run`: one download, in a fresh process)
//! ```
//! `run` starts the test server as a separate process (its CPU is not counted), then for every size and connection
//! count downloads the file with the real engine and its SQLite store, each in a fresh process, and reports MB/s, the engine's CPU time and
//! peak memory, the time spent saving progress to the database, and the time after the last byte (flush, checks,
//! rename). Every file is checked against the SHA-256 of what the server sent, then deleted. A size that does not fit
//! on the disk (with 10 % to spare) is skipped and said so.
//!
//! The server sends a position-dependent pattern computed on the fly (no file, no memory for 50 GB), so a byte written
//! at the wrong place changes the hash. `--per-conn <MB/s>` caps each connection, like a CDN that limits per
//! connection: it shows what more connections bring on a real line; without it, the loopback shows the engine's own
//! ceiling.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use downloader_core::{
    ClientConfig, DownloadConfig, DownloadControl, DownloadRecord, DownloadRequest, DownloadStatus, DownloadStore, Downloader,
    ProgressHandle, Segment,
};
use persistence::SqliteStore;
use sha2::{Digest, Sha256};

const BLOCK: usize = 1 << 20;

/// The served bytes: a 1 MiB random block, rotated differently for every MiB of the file.
struct Pattern {
    base: Vec<u8>,
}

impl Pattern {
    fn new() -> Self {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let base = (0..BLOCK)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect();
        Self { base }
    }

    fn fill(&self, mut offset: u64, out: &mut [u8]) {
        let mut i = 0;
        while i < out.len() {
            let k = offset / BLOCK as u64;
            let within = (offset % BLOCK as u64) as usize;
            let rot = (k.wrapping_mul(7919).wrapping_add(13) % BLOCK as u64) as usize;
            let start = (within + rot) % BLOCK;
            let n = (out.len() - i).min(BLOCK - within).min(BLOCK - start);
            out[i..i + n].copy_from_slice(&self.base[start..start + n]);
            i += n;
            offset += n as u64;
        }
    }

    fn sha256(&self, size: u64) -> [u8; 32] {
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 4 * BLOCK];
        let mut at = 0;
        while at < size {
            let n = (size - at).min(buf.len() as u64) as usize;
            self.fill(at, &mut buf[..n]);
            h.update(&buf[..n]);
            at += n as u64;
        }
        h.finalize().into()
    }
}

// ------------------------------------------------------------------------------------------------- server

fn serve(per_conn_bps: u64) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    println!("PORT {}", listener.local_addr().unwrap().port());
    std::io::stdout().flush().unwrap();
    // Ends with the parent: it holds our stdin.
    std::thread::spawn(|| {
        let mut sink = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut sink);
        std::process::exit(0);
    });
    let pattern = Arc::new(Pattern::new());
    for conn in listener.incoming().flatten() {
        let p = pattern.clone();
        std::thread::spawn(move || {
            let _ = handle(conn, &p, per_conn_bps);
        });
    }
}

fn handle(conn: TcpStream, p: &Pattern, per_conn_bps: u64) -> std::io::Result<()> {
    conn.set_nodelay(true)?;
    let mut reader = BufReader::new(conn.try_clone()?);
    let mut out = conn;
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let mut parts = line.split_whitespace();
        let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
        let mut range: Option<(u64, Option<u64>)> = None;
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h)? == 0 {
                return Ok(());
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some(v) = h.strip_prefix("Range: bytes=").or_else(|| h.strip_prefix("range: bytes=")) {
                if let Some((a, b)) = v.split_once('-') {
                    range = a.parse().ok().map(|a| (a, b.parse().ok()));
                }
            }
        }
        let Some(size) = path.strip_prefix("/file/").and_then(|s| s.parse::<u64>().ok()) else {
            out.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")?;
            continue;
        };
        let (start, end, status) = match range {
            Some((a, b)) if a < size => (a, b.unwrap_or(size - 1).min(size - 1), 206),
            _ => (0, size - 1, 200),
        };
        let mut head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: application/octet-stream\r\nAccept-Ranges: bytes\r\nETag: \"bench-{size}\"\r\nContent-Length: {}\r\n",
            status,
            if status == 206 { "Partial Content" } else { "OK" },
            end - start + 1
        );
        if status == 206 {
            head.push_str(&format!("Content-Range: bytes {start}-{end}/{size}\r\n"));
        }
        head.push_str("\r\n");
        out.write_all(head.as_bytes())?;
        if method == "HEAD" {
            continue;
        }
        let begun = Instant::now();
        let mut at = start;
        while at <= end {
            let n = (end + 1 - at).min(buf.len() as u64) as usize;
            p.fill(at, &mut buf[..n]);
            out.write_all(&buf[..n])?;
            at += n as u64;
            if per_conn_bps > 0 {
                let due = Duration::from_secs_f64((at - start) as f64 / per_conn_bps as f64);
                if let Some(wait) = due.checked_sub(begun.elapsed()) {
                    std::thread::sleep(wait);
                }
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------- the store, timed

/// The engine's real SQLite store, with the time spent in it counted.
struct TimedStore {
    inner: Arc<SqliteStore>,
    saves: AtomicU64,
    nanos: AtomicU64,
}

impl TimedStore {
    fn timed<T>(&self, f: impl FnOnce() -> T) -> T {
        let t = Instant::now();
        let r = f();
        self.saves.fetch_add(1, Ordering::Relaxed);
        self.nanos.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
        r
    }
}

impl DownloadStore for TimedStore {
    fn load(&self, id: i64) -> downloader_core::Result<Option<DownloadRecord>> {
        self.inner.load(id)
    }
    fn save(&self, rec: &DownloadRecord) -> downloader_core::Result<i64> {
        self.timed(|| self.inner.save(rec))
    }
    fn complete(&self, id: i64) -> downloader_core::Result<()> {
        self.timed(|| self.inner.complete(id))
    }
    fn destination_in_use(&self, destination: &Path, except: Option<i64>) -> downloader_core::Result<bool> {
        self.inner.destination_in_use(destination, except)
    }
}

// ------------------------------------------------------------------------------------------------- the client

struct Case {
    size: u64,
    conns: usize,
    adaptive: bool,
    /// Per-connection write buffer (`DownloadConfig::buffer_size`); `None`: the engine's default.
    buffer: Option<usize>,
}

#[derive(Debug)]
struct Outcome {
    secs: f64,
    mbps: f64,
    cpu_secs: f64,
    cpu_per_gb: f64,
    peak_rss_mb: f64,
    connections: usize,
    db_saves: u64,
    db_ms: f64,
    tail_ms: f64,
    ok: bool,
}

fn own_process(sys: &mut sysinfo::System) -> (u64, u64) {
    let pid = sysinfo::get_current_pid().expect("pid");
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing().with_cpu().with_memory(),
    );
    sys.process(pid).map(|p| (p.accumulated_cpu_time(), p.memory())).unwrap_or((0, 0))
}

async fn run_case(port: u16, case: &Case, dir: &Path, expected: [u8; 32]) -> Result<Outcome, String> {
    let work = dir.join(format!("case-{}-{}-{}", case.size, case.conns, case.adaptive));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let store = Arc::new(TimedStore {
        inner: Arc::new(SqliteStore::open(work.join("state.db")).map_err(|e| e.to_string())?),
        saves: AtomicU64::new(0),
        nanos: AtomicU64::new(0),
    });
    let mut cfg = DownloadConfig { max_connections: case.conns, mark_of_the_web: false, ..Default::default() };
    cfg.adaptive.enabled = case.adaptive;
    if let Some(b) = case.buffer {
        cfg.buffer_size = b;
    }
    let d = Downloader::new(&ClientConfig::default(), cfg).map_err(|e| e.to_string())?.with_store(store.clone() as Arc<dyn DownloadStore>);
    let mut req = DownloadRequest::new(format!("http://127.0.0.1:{port}/file/{}", case.size), &work);
    req.filename = Some("bench.bin".into());

    let progress = ProgressHandle::new();
    let peak = Arc::new(AtomicU64::new(0));
    let last_byte = Arc::new(std::sync::Mutex::new(None::<Instant>));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sampler = {
        let (peak, last_byte, stop, progress, size) = (peak.clone(), last_byte.clone(), stop.clone(), progress.clone(), case.size);
        std::thread::spawn(move || {
            let mut sys = sysinfo::System::new();
            while !stop.load(Ordering::Relaxed) {
                let (_, mem) = own_process(&mut sys);
                peak.fetch_max(mem, Ordering::Relaxed);
                let mut lb = last_byte.lock().unwrap();
                if lb.is_none() && progress.downloaded() >= size {
                    *lb = Some(Instant::now());
                }
                drop(lb);
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    };
    let mut sys = sysinfo::System::new();
    let (cpu0, _) = own_process(&mut sys);
    let t = Instant::now();
    let outcome = d.download(&req, DownloadControl::new(), progress.clone()).await;
    let ended = Instant::now();
    let secs = ended.duration_since(t).as_secs_f64();
    let (cpu1, _) = own_process(&mut sys);
    stop.store(true, Ordering::Relaxed);
    let _ = sampler.join();
    let outcome = outcome.map_err(|e| e.to_string())?;
    // From the last byte received to the finished file (flush, checks, rename, the final save).
    let tail_ms = last_byte.lock().unwrap().map_or(0.0, |lb| ended.saturating_duration_since(lb).as_secs_f64() * 1000.0);
    // Check every byte, outside the timing.
    let path = outcome.path.clone();
    let got = tokio::task::spawn_blocking(move || -> std::io::Result<[u8; 32]> {
        let mut f = std::fs::File::open(path)?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 4 * BLOCK];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        Ok(h.finalize().into())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&work);
    let cpu_secs = cpu1.saturating_sub(cpu0) as f64 / 1000.0;
    let gb = case.size as f64 / (1u64 << 30) as f64;
    Ok(Outcome {
        secs,
        mbps: case.size as f64 / (1 << 20) as f64 / secs,
        cpu_secs,
        cpu_per_gb: cpu_secs / gb,
        peak_rss_mb: peak.load(Ordering::Relaxed) as f64 / (1 << 20) as f64,
        connections: outcome.connections,
        db_saves: store.saves.load(Ordering::Relaxed),
        db_ms: store.nanos.load(Ordering::Relaxed) as f64 / 1e6,
        tail_ms,
        ok: got == expected && outcome.size == case.size,
    })
}

/// `velox-bench case ...` in a child process; its last output line is the outcome as JSON.
fn run_in_child(port: u16, case: &Case, dir: &Path, expected: [u8; 32]) -> Result<Outcome, String> {
    let mut cmd = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    cmd.args(["case", "--port", &port.to_string(), "--size", &case.size.to_string(), "--conns", &case.conns.to_string()]);
    cmd.args(["--dir", &dir.to_string_lossy(), "--sha", &expected.iter().map(|b| format!("{b:02x}")).collect::<String>()]);
    if case.adaptive {
        cmd.arg("--adaptive");
    }
    if let Some(b) = case.buffer {
        cmd.args(["--buffer", &b.to_string()]);
    }
    let out = cmd.stderr(Stdio::inherit()).output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().last().unwrap_or("");
    let v: serde_json::Value = serde_json::from_str(line).map_err(|_| format!("the case failed: {text}"))?;
    if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
        return Err(e.to_string());
    }
    let f = |k: &str| v[k].as_f64().unwrap_or(0.0);
    Ok(Outcome {
        secs: f("secs"),
        mbps: f("mbps"),
        cpu_secs: f("cpu_secs"),
        cpu_per_gb: f("cpu_per_gb"),
        peak_rss_mb: f("peak_rss_mb"),
        connections: f("connections") as usize,
        db_saves: f("db_saves") as u64,
        db_ms: f("db_ms"),
        tail_ms: f("tail_ms"),
        ok: v["ok"].as_bool().unwrap_or(false),
    })
}

/// One case, in this (child) process: prints its outcome as one JSON line.
async fn case_main(args: &Args) {
    let num = |k: &str| args.get(k).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    let case = Case {
        size: num("--size"),
        conns: num("--conns") as usize,
        adaptive: args.has("--adaptive"),
        buffer: args.get("--buffer").and_then(|b| b.parse().ok()),
    };
    let sha = args.get("--sha").unwrap_or("");
    let mut expected = [0u8; 32];
    for (i, b) in expected.iter_mut().enumerate() {
        *b = u8::from_str_radix(sha.get(i * 2..i * 2 + 2).unwrap_or("00"), 16).unwrap_or(0);
    }
    let dir = PathBuf::from(args.get("--dir").unwrap_or("target/bench-work"));
    let line = match run_case(num("--port") as u16, &case, &dir, expected).await {
        Ok(o) => serde_json::json!({
            "secs": o.secs, "mbps": o.mbps, "cpu_secs": o.cpu_secs, "cpu_per_gb": o.cpu_per_gb, "peak_rss_mb": o.peak_rss_mb,
            "connections": o.connections, "db_saves": o.db_saves, "db_ms": o.db_ms, "tail_ms": o.tail_ms, "ok": o.ok,
        }),
        Err(e) => serde_json::json!({ "error": e }),
    };
    println!("{line}");
}

fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_uppercase();
    let (n, m) = match s.chars().last()? {
        'K' => (&s[..s.len() - 1], 1u64 << 10),
        'M' => (&s[..s.len() - 1], 1 << 20),
        'G' => (&s[..s.len() - 1], 1 << 30),
        _ => (s.as_str(), 1),
    };
    n.parse::<f64>().ok().map(|v| (v * m as f64) as u64)
}

fn human(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.0} GiB", b as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.0} MiB", b as f64 / (1 << 20) as f64)
    }
}

fn free_space(dir: &Path) -> Option<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let dir = dir.canonicalize().ok()?;
    disks
        .list()
        .iter()
        .filter(|d| dir.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
}

struct Args(Vec<String>);

impl Args {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.iter().position(|a| a == name).and_then(|i| self.0.get(i + 1)).map(String::as_str)
    }
    fn has(&self, name: &str) -> bool {
        self.0.iter().any(|a| a == name)
    }
}

async fn run(args: &Args) {
    let sizes: Vec<u64> = args.get("--sizes").unwrap_or("100M,1G").split(',').filter_map(parse_size).collect();
    let conns: Vec<usize> = args.get("--conns").unwrap_or("1,4,8,16,32,64").split(',').filter_map(|c| c.parse().ok()).collect();
    let repeat: usize = args.get("--repeat").and_then(|r| r.parse().ok()).unwrap_or(1).max(1);
    let per_conn_mbps: f64 = args.get("--per-conn").and_then(|r| r.parse().ok()).unwrap_or(0.0);
    let dir = PathBuf::from(args.get("--dir").unwrap_or("target/bench-work"));
    let buffer: Option<usize> = args.get("--buffer-kib").and_then(|b| b.parse::<usize>().ok()).map(|k| k * 1024);
    std::fs::create_dir_all(&dir).expect("work folder");

    let exe = std::env::current_exe().expect("exe");
    let mut server = Command::new(exe)
        .args(["serve", "--per-conn-bps", &((per_conn_mbps * 1024.0 * 1024.0) as u64).to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("server");
    let mut first = String::new();
    BufReader::new(server.stdout.take().unwrap()).read_line(&mut first).expect("port");
    let port: u16 = first.trim().strip_prefix("PORT ").and_then(|p| p.parse().ok()).expect("port line");

    let mut cases = Vec::new();
    for &size in &sizes {
        for &c in &conns {
            cases.push(Case { size, conns: c, adaptive: false, buffer });
        }
        if args.has("--adaptive") {
            cases.push(Case { size, conns: 64, adaptive: true, buffer });
        }
    }
    println!("| size | connections | mode | time (s) | MiB/s | CPU (s) | CPU s / GiB | peak RAM (MiB) | used | DB saves | DB time (ms) | after last byte (ms) | file |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    let mut json = Vec::new();
    let pattern = Pattern::new();
    let mut hashes = std::collections::HashMap::new();
    for case in &cases {
        if free_space(&dir).is_some_and(|free| free < case.size + case.size / 10) {
            println!("| {} | {} | — | skipped: not enough free disk space | | | | | | | | | |", human(case.size), case.conns);
            continue;
        }
        let expected = *hashes.entry(case.size).or_insert_with(|| pattern.sha256(case.size));
        let mut runs = Vec::new();
        for _ in 0..repeat {
            // Each run in a fresh process: its CPU time and peak memory are its own, not left over from an earlier case.
            match run_in_child(port, case, &dir, expected) {
                Ok(o) => runs.push(o),
                Err(e) => println!("| {} | {} | error: {e} |", human(case.size), case.conns),
            }
        }
        // The median run by time.
        runs.sort_by(|a, b| a.secs.total_cmp(&b.secs));
        let Some(o) = runs.get(runs.len() / 2) else { continue };
        let mode = if case.adaptive { "adaptive (≤64)" } else { "fixed" };
        println!(
            "| {} | {} | {mode} | {:.2} | {:.0} | {:.2} | {:.2} | {:.0} | {} | {} | {:.0} | {:.0} | {} |",
            human(case.size),
            case.conns,
            o.secs,
            o.mbps,
            o.cpu_secs,
            o.cpu_per_gb,
            o.peak_rss_mb,
            o.connections,
            o.db_saves,
            o.db_ms,
            o.tail_ms,
            if o.ok { "exact" } else { "MISMATCH" }
        );
        json.push(serde_json::json!({
            "size": case.size, "connections": case.conns, "adaptive": case.adaptive, "perConnMBps": per_conn_mbps,
            "secs": o.secs, "mibps": o.mbps, "cpuSecs": o.cpu_secs, "cpuSecsPerGiB": o.cpu_per_gb, "peakRssMiB": o.peak_rss_mb,
            "connectionsUsed": o.connections, "dbSaves": o.db_saves, "dbMs": o.db_ms, "afterLastByteMs": o.tail_ms, "exact": o.ok,
        }));
        if !o.ok {
            eprintln!("velox-bench: the downloaded file does not match what the server sent");
            std::process::exit(2);
        }
    }
    if let Some(path) = args.get("--json") {
        std::fs::write(path, serde_json::to_string_pretty(&json).unwrap()).expect("json");
    }
    drop(server.stdin.take());
    let _ = server.wait();
}

/// The cost of one progress save: a record with `segments` segments, saved `saves` times (what the engine does at
/// every checkpoint, every 500 ms per running download).
fn db(args: &Args) {
    let segments: usize = args.get("--segments").and_then(|s| s.parse().ok()).unwrap_or(64);
    let saves: usize = args.get("--saves").and_then(|s| s.parse().ok()).unwrap_or(2000);
    let dir = std::env::temp_dir().join(format!("velox-bench-db-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let store = SqliteStore::open(dir.join("state.db")).unwrap();
    let step = 1u64 << 24;
    let mut rec = DownloadRecord {
        id: None,
        url: "http://127.0.0.1/file".into(),
        filename: "bench.bin".into(),
        destination: dir.join("bench.bin"),
        total_size: Some(step * segments as u64),
        downloaded: 0,
        status: DownloadStatus::Downloading,
        etag: Some("\"bench\"".into()),
        last_modified: None,
        resumable: true,
        checksum: None,
        last_error: None,
        segments: (0..segments).map(|i| Segment::new(i, i as u64 * step, (i as u64 + 1) * step - 1)).collect(),
        priority: Default::default(),
        category: None,
        speed_limit: None,
        content_type: None,
        server: None,
        request_headers: Vec::new(),
        part_stamp: None,
    };
    rec.id = Some(store.save(&rec).unwrap());
    let mut times = Vec::with_capacity(saves);
    for n in 0..saves {
        for s in rec.segments.iter_mut() {
            s.downloaded = ((n as u64 + 1) * 4096).min(step);
        }
        rec.downloaded = rec.segments.iter().map(|s| s.downloaded).sum();
        let t = Instant::now();
        store.save(&rec).unwrap();
        times.push(t.elapsed().as_secs_f64() * 1e6);
    }
    times.sort_by(f64::total_cmp);
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    println!(
        "progress save, {segments} segments, {saves} saves: mean {mean:.0} µs, median {:.0} µs, p95 {:.0} µs, max {:.0} µs",
        times[times.len() / 2],
        times[times.len() * 95 / 100],
        times[times.len() - 1]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn main() {
    let args = Args(std::env::args().skip(1).collect());
    match args.0.first().map(String::as_str) {
        Some("serve") => serve(args.get("--per-conn-bps").and_then(|v| v.parse().ok()).unwrap_or(0)),
        Some("run") => tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap().block_on(run(&args)),
        Some("case") => tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap().block_on(case_main(&args)),
        Some("db") => db(&args),
        _ => {
            eprintln!("usage: velox-bench run|db [options]  (see the top of tests/bench/src/main.rs, docs/benchmarks.md)");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pattern_depends_on_the_position_and_ranges_agree_with_the_whole() {
        let p = Pattern::new();
        let mut whole = vec![0u8; 3 * BLOCK + 17];
        p.fill(0, &mut whole);
        // A range taken anywhere is the same bytes as in the whole file.
        for (a, n) in [(0usize, 10usize), (BLOCK - 5, 20), (2 * BLOCK + 3, BLOCK)] {
            let mut part = vec![0u8; n];
            p.fill(a as u64, &mut part);
            assert_eq!(part, whole[a..a + n]);
        }
        // Two MiB blocks are not the same bytes: a block written at the wrong place changes the hash.
        assert_ne!(whole[..BLOCK], whole[BLOCK..2 * BLOCK]);
        assert_eq!(p.sha256(whole.len() as u64), <[u8; 32]>::from(Sha256::digest(&whole)));
    }

    #[test]
    fn sizes_are_read_with_units() {
        assert_eq!(parse_size("100M"), Some(100 << 20));
        assert_eq!(parse_size("1.5G"), Some(3 << 29));
        assert_eq!(parse_size("50g"), Some(50 << 30));
        assert_eq!(parse_size("x"), None);
    }
}
