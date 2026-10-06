//! The local, per-user channel between the native host and the running app: a named pipe on
//! Windows, a Unix domain socket elsewhere. Neither is reachable from the network.

use std::path::Path;

use tokio::io::{AsyncRead, AsyncWrite};

pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send + Sync {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + Sync> Stream for T {}
pub type BoxStream = Box<dyn Stream>;

/// A pipe name (Windows) or socket path (Unix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint(pub String);

impl Endpoint {
    /// The normal endpoint for this user. `data_dir` is the app's data directory.
    pub fn default_for(data_dir: &Path) -> Self {
        #[cfg(windows)]
        {
            let _ = data_dir;
            // Per-user name; anything odd in the user name is replaced so the name stays valid.
            let user: String = std::env::var("USERNAME")
                .unwrap_or_default()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
                .take(64)
                .collect();
            Self(format!(r"\\.\pipe\velox-fetch-{user}"))
        }
        #[cfg(not(windows))]
        {
            Self(unix_socket_path(data_dir).to_string_lossy().to_string())
        }
    }
}

/// Longest socket path the system accepts (`sun_path` less its final NUL): 107 on Linux, 103 on macOS / BSD.
#[cfg(not(windows))]
const MAX_SOCKET_PATH: usize = if cfg!(target_os = "linux") { 107 } else { 103 };

/// `bridge.sock` in `dir`, or a shorter name when that would be too long for the system (a browser's folder inside
/// a Flatpak with a long id, a long user name). The app and the helper apply the same rule, so they always agree.
#[cfg(not(windows))]
pub(crate) fn unix_socket_path(dir: &Path) -> std::path::PathBuf {
    let normal = dir.join("bridge.sock");
    if normal.as_os_str().len() <= MAX_SOCKET_PATH {
        normal
    } else {
        dir.join("s")
    }
}

#[cfg(all(test, not(windows)))]
mod path_tests {
    use super::*;

    #[test]
    fn a_long_folder_gets_a_short_socket_name_on_both_sides() {
        let short = Path::new("/home/u/.local/share/app.velox.fetch");
        assert_eq!(unix_socket_path(short), short.join("bridge.sock"));
        let long = Path::new("/home/twenty-char-username/.var/app/io.github.ungoogled_software.ungoogled_chromium/data/app.velox.fetch");
        assert!(long.join("bridge.sock").as_os_str().len() > 107, "the case this guards against");
        assert_eq!(unix_socket_path(long), long.join("s"));
        assert!(unix_socket_path(long).as_os_str().len() <= MAX_SOCKET_PATH);
        assert_eq!(Endpoint::default_for(long), Endpoint::default_for(long), "the same answer every time");
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::time::Duration;

    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};

    use super::{BoxStream, Endpoint};

    const ERROR_PIPE_BUSY: i32 = 231;

    pub struct Listener {
        name: String,
        next: NamedPipeServer,
    }

    impl Listener {
        /// Fails if another instance already owns the pipe: nobody can squat on it before us.
        pub fn bind(ep: &Endpoint) -> io::Result<Self> {
            let next = ServerOptions::new().first_pipe_instance(true).reject_remote_clients(true).create(&ep.0)?;
            Ok(Self { name: ep.0.clone(), next })
        }

        pub async fn accept(&mut self) -> io::Result<BoxStream> {
            self.next.connect().await?;
            let fresh = ServerOptions::new().reject_remote_clients(true).create(&self.name)?;
            Ok(Box::new(std::mem::replace(&mut self.next, fresh)))
        }
    }

    pub async fn connect(ep: &Endpoint) -> io::Result<BoxStream> {
        let mut tries = 0;
        loop {
            match ClientOptions::new().open(&ep.0) {
                Ok(c) => return Ok(Box::new(c)),
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && tries < 20 => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::fs::PermissionsExt;

    use tokio::net::{UnixListener, UnixStream};

    use super::{BoxStream, Endpoint};

    pub struct Listener(UnixListener);

    impl Listener {
        /// Fails with `AddrInUse` if another instance is listening there: its socket is never taken over.
        pub fn bind(ep: &Endpoint) -> io::Result<Self> {
            if std::os::unix::net::UnixStream::connect(&ep.0).is_ok() {
                return Err(io::Error::new(io::ErrorKind::AddrInUse, "another instance owns the link"));
            }
            let _ = std::fs::remove_file(&ep.0); // a stale socket from a crashed run
            let l = UnixListener::bind(&ep.0)?;
            std::fs::set_permissions(&ep.0, std::fs::Permissions::from_mode(0o600))?;
            Ok(Self(l))
        }

        pub async fn accept(&mut self) -> io::Result<BoxStream> {
            let (s, _) = self.0.accept().await?;
            Ok(Box::new(s))
        }
    }

    pub async fn connect(ep: &Endpoint) -> io::Result<BoxStream> {
        Ok(Box::new(UnixStream::connect(&ep.0).await?))
    }
}

#[cfg(not(any(windows, unix)))]
mod imp {
    use std::io;

    use super::{BoxStream, Endpoint};

    pub struct Listener;

    impl Listener {
        pub fn bind(_: &Endpoint) -> io::Result<Self> {
            Err(io::Error::other("unsupported platform"))
        }
        pub async fn accept(&mut self) -> io::Result<BoxStream> {
            Err(io::Error::other("unsupported platform"))
        }
    }

    pub async fn connect(_: &Endpoint) -> io::Result<BoxStream> {
        Err(io::Error::other("unsupported platform"))
    }
}

pub use imp::{connect, Listener};

/// Did [`Listener::bind`] fail because another instance already owns the endpoint (rather than, say, a
/// path that cannot be created)?
pub fn owned_by_another_instance(e: &std::io::Error) -> bool {
    const ERROR_ACCESS_DENIED: i32 = 5; // a pipe of that name exists (first_pipe_instance)
    const ERROR_PIPE_BUSY: i32 = 231;
    e.kind() == std::io::ErrorKind::AddrInUse || (cfg!(windows) && matches!(e.raw_os_error(), Some(ERROR_ACCESS_DENIED | ERROR_PIPE_BUSY)))
}
