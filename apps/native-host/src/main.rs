//! `velox-native-host`: the native messaging host browsers start for the VeloxFetch extension.
//!
//! Speaks length-prefixed JSON on stdin/stdout (the browser side) and a local authenticated pipe
//! (the app side). **stdout carries the protocol and nothing else**: diagnostics go to stderr.

use std::time::Duration;

use browser_bridge::host::{run, HostConfig};
use browser_bridge::{data_dir, Allowlist, Caller, Endpoint};

fn main() {
    path_safety::harden_process();
    let args: Vec<String> = std::env::args().skip(1).collect();

    let caller = match Caller::from_args(&args).and_then(|c| c.check(&Allowlist::default()).map(|_| c)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("velox-native-host: {}", diagnostics::redact(&e.to_string()));
            std::process::exit(2);
        }
    };
    let Some(dir) = data_dir() else {
        eprintln!("velox-native-host: cannot locate the VeloxFetch data folder");
        std::process::exit(3);
    };
    // The desktop app lives next to this program.
    let app_exe = std::env::current_exe().ok().and_then(|p| {
        let name = if cfg!(windows) { "velox-desktop.exe" } else { "velox-desktop" };
        p.parent().map(|d| d.join(name))
    });
    let endpoint = std::env::var("VELOX_BRIDGE_ENDPOINT")
        .ok()
        .filter(|_| cfg!(debug_assertions)) // test hook: never honoured in release builds
        .map(Endpoint)
        .unwrap_or_else(|| Endpoint::default_for(&dir));
    let cfg = HostConfig { endpoint, data_dir: dir, app_exe, launch_wait: Duration::from_secs(20) };

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    let code = rt.block_on(async {
        let (mut stdin, mut stdout) = (tokio::io::stdin(), tokio::io::stdout());
        match run(&mut stdin, &mut stdout, caller, &cfg).await {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("velox-native-host: {}", diagnostics::redact(&e.to_string()));
                1
            }
        }
    });
    std::process::exit(code);
}
