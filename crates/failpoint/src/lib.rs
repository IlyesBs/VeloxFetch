//! Crash and fault hooks for the tests (Phase 13).
//!
//! `failpoint::hit("name")` marks a place where a process can die at the worst moment, and
//! `failpoint::hit_io("name")?` a place where an operation can fail (disk full, access denied...).
//! In a shipped build both are empty functions the compiler removes. With the `enabled` feature (used only
//! by the crash tests) the environment variable `VELOX_FAILPOINT` decides what happens:
//!
//! * `name` or `name@N`: the process is killed **without any cleanup** on the N-th hit of that name (default:
//!   the first): `SIGKILL` on Unix, an abort on Windows, as `kill -9` or a task manager would;
//! * `name@N:enospc`, `:denied`, `:notfound`, `:io`: from the N-th hit of `name` on, `hit_io` fails with that
//!   kind of I/O error (disk full, access denied, path gone, other), every time, like a disk that stays full.
//!
//! The environment variable is only read when the feature is on, so a shipped program cannot be told to fail.

/// Marks a crash point. Does nothing unless the `enabled` feature is on.
#[inline(always)]
pub fn hit(name: &str) {
    #[cfg(feature = "enabled")]
    let _ = imp::hit(name, false);
    #[cfg(not(feature = "enabled"))]
    let _ = name;
}

/// Marks a place where an I/O operation can fail. `Ok(())` unless a test asked otherwise (and it can also kill).
#[inline(always)]
pub fn hit_io(name: &str) -> std::io::Result<()> {
    #[cfg(feature = "enabled")]
    return imp::hit(name, true);
    #[cfg(not(feature = "enabled"))]
    {
        let _ = name;
        Ok(())
    }
}

/// True when this build can crash on purpose (only test builds).
pub const ENABLED: bool = cfg!(feature = "enabled");

#[cfg(feature = "enabled")]
mod imp {
    use std::collections::HashMap;
    use std::io::{Error, ErrorKind};
    use std::sync::{Mutex, OnceLock};

    #[derive(Clone, Copy, PartialEq)]
    enum Action {
        Kill,
        Fail(ErrorKind, &'static str),
    }

    struct Spec {
        name: String,
        nth: u64,
        action: Action,
    }

    fn spec() -> &'static Option<Spec> {
        static SPEC: OnceLock<Option<Spec>> = OnceLock::new();
        SPEC.get_or_init(|| {
            let v = std::env::var("VELOX_FAILPOINT").ok()?;
            let (rest, action) = match v.split_once(':') {
                Some((r, a)) => (
                    r.to_string(),
                    match a {
                        "enospc" => Action::Fail(ErrorKind::StorageFull, "No space left on device (injected)"),
                        "denied" => Action::Fail(ErrorKind::PermissionDenied, "Access is denied (injected)"),
                        "notfound" => Action::Fail(ErrorKind::NotFound, "The system cannot find the path specified (injected)"),
                        "io" => Action::Fail(ErrorKind::Other, "Input/output error (injected)"),
                        _ => Action::Kill,
                    },
                ),
                None => (v, Action::Kill),
            };
            let (name, nth) = match rest.split_once('@') {
                Some((n, k)) => (n.to_string(), k.parse().unwrap_or(1)),
                None => (rest, 1),
            };
            Some(Spec { name, nth: nth.max(1), action })
        })
    }

    /// `kill -9` on ourselves where there is one (no crash reporter, no handlers), an abort elsewhere.
    fn die() -> ! {
        #[cfg(unix)]
        // SAFETY: plain syscall on our own pid; the process ends here.
        unsafe {
            libc::kill(libc::getpid(), libc::SIGKILL);
        }
        std::process::abort()
    }

    pub fn hit(name: &str, io: bool) -> std::io::Result<()> {
        let Some(s) = spec() else { return Ok(()) };
        if s.name != name {
            return Ok(());
        }
        static COUNTS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
        let n = {
            let mut c = COUNTS.get_or_init(Default::default).lock().unwrap();
            let e = c.entry(name.to_string()).or_insert(0);
            *e += 1;
            *e
        };
        match s.action {
            Action::Kill if n == s.nth => {
                // Tell the parent test where we died, then die without unwinding, flushing or closing anything.
                eprintln!("VELOX_FAILPOINT_HIT {name}@{n}");
                die();
            }
            Action::Fail(kind, msg) if io && n >= s.nth => {
                if n == s.nth {
                    eprintln!("VELOX_FAILPOINT_FAIL {name}@{n}");
                }
                Err(Error::new(kind, msg))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn hits_are_harmless_without_the_environment_variable() {
        super::hit("anything");
        super::hit("anything");
        assert!(super::hit_io("anything").is_ok());
    }
}
