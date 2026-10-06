//! Smart throttle, the desktop half: every few seconds, list the running programs and tell the manager.
//! The decision (which programs, how slow, lifting the cap) lives in `download_manager`'s
//! `smart_throttle.rs`; this file only looks at the system.
//!
//! * Looking costs almost nothing: process *names* only (no CPU, memory or command lines), once every
//!   3 seconds, on a blocking thread, and not at all while the feature is off.
//! * Never in the way: if listing fails, hangs (2 s limit) or panics, the manager is told "unknown" and
//!   downloads run at full speed.
//! * Nothing leaves the computer and nothing is stored: the names are compared and dropped.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use download_manager::DownloadManager;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

const EVERY: Duration = Duration::from_secs(3);
const SCAN_LIMIT: Duration = Duration::from_secs(2);

/// Start the watcher (for the life of the app).
pub fn spawn(manager: DownloadManager) {
    tauri::async_runtime::spawn(async move {
        // Kept between scans: refreshing an existing list is much cheaper than building a new one.
        let system: Arc<Mutex<Option<System>>> = Arc::new(Mutex::new(None));
        let mut tick = tokio::time::interval(EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if !manager.smart_throttle_enabled() {
                // Off: no scan; the manager still learns how fast the connection is.
                system.lock().map(|mut s| s.take()).ok();
                manager.smart_throttle_observe(Some(&[]));
                continue;
            }
            let sys = system.clone();
            let scan = tokio::task::spawn_blocking(move || running_programs(&sys));
            match tokio::time::timeout(SCAN_LIMIT, scan).await {
                Ok(Ok(Some(names))) => manager.smart_throttle_observe(Some(&names)),
                // Failed, panicked or too slow: full speed.
                _ => manager.smart_throttle_observe(None),
            }
        }
    });
}

/// The names of the running programs, or `None` if they cannot be listed.
fn running_programs(system: &Mutex<Option<System>>) -> Option<Vec<String>> {
    let mut guard = system.lock().ok()?;
    let sys = guard.get_or_insert_with(System::new);
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let mut names: Vec<String> = sys.processes().values().map(|p| p.name().to_string_lossy().into_owned()).collect();
    names.sort_unstable();
    names.dedup();
    Some(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scan_is_quick() {
        let sys = Mutex::new(None);
        for i in 0..3 {
            let t = std::time::Instant::now();
            running_programs(&sys).unwrap();
            eprintln!("scan {i}: {:?}", t.elapsed());
        }
    }

    #[test]
    fn this_test_process_is_listed() {
        let names = running_programs(&Mutex::new(None)).expect("the process list can be read");
        let me = std::env::current_exe().unwrap().file_name().unwrap().to_string_lossy().into_owned();
        assert!(names.iter().any(|n| n.eq_ignore_ascii_case(&me) || me.starts_with(n.as_str())), "{me} not among {} names", names.len());
    }
}
