//! Smart throttle, the desktop half: every few seconds, list the running programs, ask the system how long the
//! user has been away (automatic mode), and tell the manager, which also measures the computer's traffic (its
//! network counters and VeloxFetch's own, read at the same moment, on the same blocking thread). The
//! decision (which programs, how slow, smoothing, lifting the cap) lives in `download_manager`'s
//! `smart_throttle.rs`; this file only looks at the system.
//!
//! * Looking costs almost nothing: process *names* and executable file names only (no CPU, memory or command
//!   lines; the executable is read once per process), once every 3 seconds, on a blocking thread, and not at all
//!   while the feature is off. The executable's name counts too because a program may rename itself (its main
//!   thread: Linux then shows "MainThread" or similar) and Linux cuts names to 15 characters. Full executable paths
//!   are passed on only when the user listed a path (to tell two programs with the same file name apart); like the
//!   names, they are compared and dropped.
//! * Never in the way: if listing fails, hangs (2 s limit) or panics, the manager is told "unknown": a gaming
//!   policy in force stays a few seconds more (no burst of traffic over a game), then downloads run at full speed.
//! * Nothing leaves the computer and nothing is stored: the names are compared and dropped; of the keyboard and
//!   mouse only the time since their last use is known, never what was typed or where.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use download_manager::DownloadManager;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

const EVERY: Duration = Duration::from_secs(3);
const SCAN_LIMIT: Duration = Duration::from_secs(2);

/// Start the watcher (for the life of the app). It also keeps the tray's Game mode tick in line and says when Game
/// mode's time ran out.
pub fn spawn(app: tauri::AppHandle, manager: DownloadManager) {
    tauri::async_runtime::spawn(async move {
        // When Game mode was to end, as last seen (`Some(None)`: on until turned off).
        let mut game_until: Option<Option<i64>> = None;
        // Kept between scans: refreshing an existing list is much cheaper than building a new one.
        let system: Arc<Mutex<Option<System>>> = Arc::new(Mutex::new(None));
        let mut tick = tokio::time::interval(EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let gm = manager.smart_throttle().game_mode;
            let now = gm.on.then_some(gm.until_ms);
            if let (Some(Some(until)), None) = (game_until, now) {
                // Off since the last tick, and its time had come: it ran out (the user did not turn it off).
                let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
                if now_ms + 1_000 >= until {
                    crate::desktop::notify_game_mode_ended(&app, &manager);
                }
            }
            game_until = now;
            crate::desktop::sync_tray_game(&app, gm.on, &manager.desktop_prefs().language);
            if !manager.smart_throttle_enabled() {
                // Off: no scan; the manager still learns how fast the connection is.
                system.lock().map(|mut s| s.take()).ok();
                manager.smart_throttle_observe(Some(&[]));
                continue;
            }
            let sys = system.clone();
            let st = manager.smart_throttle();
            let auto = st.auto;
            let paths = st.apps.iter().any(|a| download_manager::smart_throttle_is_app_path(a));
            let os = manager.system();
            let m = manager.clone();
            // The manager reads the network counters too: all of it on the blocking thread.
            let scan = tokio::task::spawn_blocking(move || {
                let names = running_programs(&sys, paths);
                let idle = if auto { os.0.user_idle() } else { None };
                m.smart_throttle_observe_activity(names.as_deref(), idle);
            });
            if !matches!(tokio::time::timeout(SCAN_LIMIT, scan).await, Ok(Ok(()))) {
                // Panicked or too slow: unknown (the manager lifts any cap after a short grace).
                manager.smart_throttle_observe(None);
            }
        }
    });
}

/// The names of the running programs (as the system shows them, and their executables' file names; with `paths`, the
/// executables' full paths too), or `None` if they cannot be listed. An executable the system will not show (another
/// user's, a protected one) is skipped.
fn running_programs(system: &Mutex<Option<System>>, paths: bool) -> Option<Vec<String>> {
    let mut guard = system.lock().ok()?;
    let sys = guard.get_or_insert_with(System::new);
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet));
    let mut names: Vec<String> = sys
        .processes()
        .values()
        .flat_map(|p| {
            let exe = p.exe().and_then(|e| e.file_name()).map(|n| n.to_string_lossy().into_owned());
            let path = p.exe().filter(|_| paths).map(|e| e.to_string_lossy().into_owned());
            std::iter::once(p.name().to_string_lossy().into_owned()).chain(exe).chain(path)
        })
        .collect();
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
            running_programs(&sys, false).unwrap();
            eprintln!("scan {i}: {:?}", t.elapsed());
        }
    }

    #[test]
    fn a_program_that_renamed_itself_is_found_by_its_executable() {
        // A copy of this test binary under a game's name, with its main thread renamed (as Node, some engines and
        // launchers do): the process name no longer says what it is; the executable's name does.
        let dir = std::env::temp_dir().join(format!("velox-scan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let name = if cfg!(windows) { "VeloxScanTestGame.exe" } else { "VeloxScanTestGame" };
        let copy = dir.join(name);
        std::fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();
        // Runs this module's idle helper test only (it waits, renamed, until killed).
        let mut child = std::process::Command::new(&copy)
            .args(["--exact", "smart_throttle::tests::idle_helper", "--ignored", "--nocapture"])
            .env("VELOX_IDLE_HELPER", "1")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let sys = Mutex::new(None);
        let apps = vec!["VeloxScanTestGame".to_string()];
        let mut found = false;
        for _ in 0..40 {
            let names = running_programs(&sys, false).unwrap();
            if download_manager::smart_throttle_find_running(&apps, &names).is_some() {
                found = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(found, "the renamed program was not found by its executable's name");
    }

    #[test]
    fn a_listed_path_finds_that_executable_and_paths_are_read_only_when_listed() {
        let me = std::env::current_exe().unwrap();
        // macOS reports the real path (no symbolic links); Windows' canonical form would add `\\?\`.
        #[cfg(target_os = "macos")]
        let me = std::fs::canonicalize(&me).unwrap();
        let sys = Mutex::new(None);
        let with = running_programs(&sys, true).unwrap();
        let listed = vec![me.to_string_lossy().into_owned()];
        assert!(download_manager::smart_throttle_find_running(&listed, &with).is_some(), "this test, by its full path");
        let elsewhere = vec![me.with_file_name("other-folder").join(me.file_name().unwrap()).to_string_lossy().into_owned()];
        assert!(download_manager::smart_throttle_find_running(&elsewhere, &with).is_none(), "same file name, other folder");
        let without = running_programs(&sys, false).unwrap();
        assert!(!without.contains(&listed[0]), "no full path is passed on unless a path is listed");
    }

    /// Not a test: the stand-in program of the test above (waits with a renamed main thread).
    #[test]
    #[ignore]
    fn idle_helper() {
        if std::env::var_os("VELOX_IDLE_HELPER").is_none() {
            return;
        }
        #[cfg(target_os = "linux")]
        std::fs::write("/proc/self/comm", "MainThread").ok();
        std::thread::sleep(Duration::from_secs(20));
    }

    #[test]
    fn this_test_process_is_listed() {
        let names = running_programs(&Mutex::new(None), false).expect("the process list can be read");
        let me = std::env::current_exe().unwrap().file_name().unwrap().to_string_lossy().into_owned();
        assert!(names.iter().any(|n| n.eq_ignore_ascii_case(&me) || me.starts_with(n.as_str())), "{me} not among {} names", names.len());
    }
}
