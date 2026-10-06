//! Optional remuxing with an ffmpeg the user already has.
//!
//! ```text
//! Rust -> validated arguments (an array, never a shell string) -> ffmpeg process
//! ```
//! Never `format!("ffmpeg {}", anything)`: there is no shell, so nothing in a file name or in a
//! manifest can become a command. Beyond that:
//! * the program is found by *absolute* path (next to the app, then `PATH` entries that are
//!   absolute), never through the current directory;
//! * every path passed is absolute, so it can never be read as an option;
//! * the input format is **forced** (`-f mpegts` / `-f mov` ...). Without that, ffmpeg guesses the
//!   format from the bytes, and a "video" that is really an HLS playlist or a `concat` script could
//!   make it open other local files or network addresses;
//! * `-protocol_whitelist file` limits it to plain files, no stdin, no overwriting, a timeout, and
//!   it is killed if the job is cancelled.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::{limits, Container, MediaError, Result};

/// Where ffmpeg is: a copy next to the app (bundled), else the first absolute `PATH` entry that has
/// one. `None` = not installed; downloads then keep the tracks as separate files.
pub fn find_ffmpeg(app_dir: Option<&Path>) -> Option<PathBuf> {
    find_program(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" }, app_dir)
}

/// `name` next to the app, else in the first absolute `PATH` entry that has it (never the current folder).
pub(crate) fn find_program(name: &str, app_dir: Option<&Path>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(d) = app_dir {
        candidates.push(d.join(name));
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).filter(|p| p.is_absolute()).map(|p| p.join(name)));
    }
    candidates.into_iter().find(|c| is_runnable(c))
}

fn is_runnable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else { return false };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// One input file for ffmpeg: where it is and what it is (decided by the manifest, not by ffmpeg).
#[derive(Clone, Copy)]
pub(crate) struct Input<'a> {
    pub path: &'a Path,
    pub container: Container,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Output {
    Mp4,
    Mkv,
    Webm,
}

impl Output {
    pub fn extension(self) -> &'static str {
        match self {
            Output::Mp4 => "mp4",
            Output::Mkv => "mkv",
            Output::Webm => "webm",
        }
    }
}

/// The output container when the user did not choose: WebM contents (VP9 / Opus) go into Matroska,
/// everything else into MP4.
pub(crate) fn output_for(inputs: &[Input]) -> Output {
    if inputs.iter().any(|i| i.container == Container::Webm) {
        Output::Mkv
    } else {
        Output::Mp4
    }
}

/// The containers to try, in order, for the user's choice: the chosen one first, then Matroska
/// (which holds anything) if the tracks do not fit it. WebM is tried only for WebM tracks.
pub(crate) fn outputs_to_try(inputs: &[Input], wanted: Option<crate::OutputFormat>) -> Vec<Output> {
    use crate::OutputFormat as F;
    let all_webm = inputs.iter().all(|i| i.container == Container::Webm);
    let first = match wanted {
        Some(F::Mp4) => Output::Mp4,
        Some(F::Mkv) => Output::Mkv,
        Some(F::Webm) if all_webm => Output::Webm,
        _ => output_for(inputs),
    };
    if first == Output::Mkv {
        vec![Output::Mkv]
    } else {
        vec![first, Output::Mkv]
    }
}

fn checked_path(p: &Path) -> Result<OsString> {
    let s = p.as_os_str();
    let lossy = s.to_string_lossy();
    if !p.is_absolute() || lossy.contains('\0') || lossy.starts_with('-') {
        return Err(MediaError::Remux("internal error: a file path is not absolute".into()));
    }
    Ok(s.to_os_string())
}

/// The full argument list, built as an array. `video` (may hold audio too) and `audio` are muxed
/// into `out` without re-encoding.
pub(crate) fn mux_args(video: Option<Input>, audio: Option<Input>, out: &Path, format: Output) -> Result<Vec<OsString>> {
    let mut a: Vec<OsString> =
        ["-nostdin", "-hide_banner", "-loglevel", "error", "-n", "-protocol_whitelist", "file"].iter().map(OsString::from).collect();
    let mut inputs: Vec<Input> = Vec::new();
    inputs.extend(video);
    inputs.extend(audio);
    if inputs.is_empty() {
        return Err(MediaError::Remux("internal error: nothing to combine".into()));
    }
    for i in &inputs {
        let fmt = i.container.ffmpeg_format().ok_or_else(|| MediaError::Remux("internal error: this track cannot be remuxed".into()))?;
        a.extend(["-f", fmt, "-i"].map(OsString::from));
        a.push(checked_path(i.path)?);
    }
    a.extend(["-map", "0"].map(OsString::from));
    if inputs.len() > 1 {
        a.extend(["-map", "1:a"].map(OsString::from));
    }
    a.extend(["-c", "copy"].map(OsString::from));
    match format {
        Output::Mp4 => a.extend(["-movflags", "+faststart", "-f", "mp4"].map(OsString::from)),
        Output::Mkv => a.extend(["-f", "matroska"].map(OsString::from)),
        Output::Webm => a.extend(["-f", "webm"].map(OsString::from)),
    }
    a.push(checked_path(out)?);
    Ok(a)
}

/// A found ffmpeg.
#[derive(Debug, Clone)]
pub struct Remux {
    program: PathBuf,
}

impl Remux {
    pub fn new(program: PathBuf) -> Self {
        Self { program }
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub(crate) async fn mux(
        &self,
        video: Option<Input<'_>>,
        audio: Option<Input<'_>>,
        out: &Path,
        format: Output,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let args = mux_args(video, audio, out, format)?;
        run_process(&self.program, &args, Duration::from_secs(limits::REMUX_TIMEOUT_SECS), cancel).await
    }
}

/// Start a program with an argument array and wait for it, with a timeout and cancellation.
/// stdin is closed; only a short tail of stderr is kept (for the error message).
pub(crate) async fn run_process(program: &Path, args: &[OsString], timeout: Duration, cancel: &CancellationToken) -> Result<()> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashes up
    let mut child = cmd.spawn().map_err(|e| MediaError::Remux(format!("could not start a required dependency: {e}")))?;
    let mut stderr = child.stderr.take();
    let tail = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(s) = stderr.as_mut() {
            let _ = s.take(8 * 1024).read_to_end(&mut buf).await;
            // Keep draining so ffmpeg never blocks on a full pipe.
            let mut sink = [0u8; 4096];
            while matches!(s.read(&mut sink).await, Ok(n) if n > 0) {}
        }
        String::from_utf8_lossy(&buf).into_owned()
    });
    let status = tokio::select! {
        r = child.wait() => r.map_err(|e| MediaError::Remux(e.to_string()))?,
        _ = tokio::time::sleep(timeout) => {
            let _ = child.kill().await;
            return Err(MediaError::Remux("joining video and sound took too long and was stopped".into()));
        }
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            return Err(MediaError::Cancelled);
        }
    };
    if status.success() {
        return Ok(());
    }
    let msg = tail.await.unwrap_or_default();
    let last = msg.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    Err(MediaError::Remux(format!(
        "joining video and sound stopped with {status}{}",
        if last.is_empty() { String::new() } else { format!(": {last}") }
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(p: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:\\media\\{p}"))
        } else {
            PathBuf::from(format!("/media/{p}"))
        }
    }

    fn strings(a: &[OsString]) -> Vec<String> {
        a.iter().map(|s| s.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn arguments_are_an_array_with_forced_formats() {
        let (v, a, o) = (abs("v.ts"), abs("a.aac"), abs("out.mp4.part"));
        let args = mux_args(
            Some(Input { path: &v, container: Container::Ts }),
            Some(Input { path: &a, container: Container::Aac }),
            &o,
            Output::Mp4,
        )
        .unwrap();
        let s = strings(&args);
        assert_eq!(&s[..7], ["-nostdin", "-hide_banner", "-loglevel", "error", "-n", "-protocol_whitelist", "file"]);
        assert!(s.windows(3).any(|w| w == ["-f", "mpegts", "-i"]) && s.windows(3).any(|w| w == ["-f", "aac", "-i"]));
        assert!(s.windows(2).any(|w| w == ["-map", "1:a"]) && s.windows(2).any(|w| w == ["-c", "copy"]));
        assert_eq!(s.last().unwrap(), &o.to_string_lossy());
        // Each path is its own argument, untouched: a name with spaces or shell characters stays one argument.
        let odd = abs("a b; rm -rf x $(id).ts");
        let s = strings(&mux_args(Some(Input { path: &odd, container: Container::Ts }), None, &o, Output::Mp4).unwrap());
        assert!(s.contains(&odd.to_string_lossy().into_owned()));
        assert!(!s.iter().any(|x| x == "1:a"), "no audio mapping without a second input");
    }

    /// Whatever text ends up in a file name, ffmpeg receives the same fixed options and the name as
    /// exactly one argument: no name can add, remove or split an option.
    #[test]
    fn no_file_name_can_inject_an_ffmpeg_argument() {
        let hostile = [
            "-i",
            "-f mp4",
            "--help",
            "-y",
            "-filter_complex",
            "-protocol_whitelist all",
            "a b",
            "a\tb",
            "a\nb",
            "a\r\nb",
            "a;b",
            "a&b",
            "a|b",
            "a`id`b",
            "$(id)",
            "${IFS}",
            "%s%s%s",
            "a\"b",
            "a'b",
            "a\\b",
            "concat:/etc/passwd",
            "file:x",
            "pipe:1",
            "http://evil/x",
            "tcp://127.0.0.1:1",
            "udp://x",
            "subfile,,start,0,end,1,,:x",
            "x.ts -map 0:v",
            "\u{202e}gpj.exe",
            "\u{ff0d}i",
            "=",
            "@",
            "~",
            "*",
            "?",
            "[a]",
            "{a,b}",
            "",
            ".",
            "..",
        ];
        let out = abs("out.mp4.part");
        let baseline = mux_args(Some(Input { path: &abs("v.ts"), container: Container::Ts }), None, &out, Output::Mp4).unwrap().len();
        for h in hostile {
            // Names are made absolute by the caller (the app), so a leading "-" can only appear after a folder.
            let p = abs(&format!("{h}.ts"));
            let args = mux_args(Some(Input { path: &p, container: Container::Ts }), None, &out, Output::Mp4).unwrap();
            assert_eq!(args.len(), baseline, "the name {h:?} changed the number of arguments");
            let s = strings(&args);
            assert_eq!(s.iter().filter(|x| **x == p.to_string_lossy()).count(), 1, "{h:?}: the name is exactly one argument");
            let i = s.iter().position(|x| x == "-i").unwrap();
            assert_eq!(s[i + 1], p.to_string_lossy(), "{h:?}: the argument after -i is the file");
            assert_eq!(&s[..7], ["-nostdin", "-hide_banner", "-loglevel", "error", "-n", "-protocol_whitelist", "file"], "{h:?}");
            // Every argument that starts with '-' is one of ours.
            let ours = ["-nostdin", "-hide_banner", "-loglevel", "-n", "-protocol_whitelist", "-f", "-i", "-map", "-c", "-movflags"];
            assert!(
                s.iter().filter(|x| x.starts_with('-')).all(|x| ours.contains(&x.as_str())),
                "{h:?}: a foreign option appeared in {s:?}"
            );
        }
        // A path that is not absolute (or starts with a dash) never reaches the program at all.
        for bad in ["-i", "--help", "-f mp4", "x.ts", "./x.ts", "..", "a/b.ts", ""] {
            let p = PathBuf::from(bad);
            assert!(mux_args(Some(Input { path: &p, container: Container::Ts }), None, &out, Output::Mp4).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn relative_and_option_like_paths_are_refused() {
        let o = abs("o.mp4");
        for bad in ["-i", "rel/x.ts", "x.ts", "--help"] {
            let p = PathBuf::from(bad);
            assert!(mux_args(Some(Input { path: &p, container: Container::Ts }), None, &o, Output::Mp4).is_err(), "{bad}");
        }
        assert!(mux_args(None, None, &o, Output::Mp4).is_err());
        let p = abs("x.vtt");
        assert!(
            mux_args(Some(Input { path: &p, container: Container::Vtt }), None, &o, Output::Mp4).is_err(),
            "subtitles are never remuxed"
        );
    }

    #[test]
    fn webm_goes_to_matroska() {
        let p = abs("v.webm");
        let inputs = [Input { path: &p, container: Container::Webm }];
        assert_eq!(output_for(&inputs), Output::Mkv);
        let p2 = abs("v.mp4");
        assert_eq!(output_for(&[Input { path: &p2, container: Container::Fmp4 }]), Output::Mp4);
        let s = strings(&mux_args(Some(inputs[0]), None, &abs("o.mkv"), Output::Mkv).unwrap());
        assert!(s.windows(2).any(|w| w == ["-f", "matroska"]) && !s.contains(&"+faststart".to_string()));
    }

    #[test]
    fn the_chosen_type_is_tried_first_then_matroska() {
        use crate::OutputFormat as F;
        let (w, m) = (abs("v.webm"), abs("v.mp4"));
        let webm = [Input { path: &w, container: Container::Webm }];
        let mp4 = [Input { path: &m, container: Container::Fmp4 }];
        assert_eq!(outputs_to_try(&webm, Some(F::Mp4)), [Output::Mp4, Output::Mkv], "VP9 can go into MP4; MKV if not");
        assert_eq!(outputs_to_try(&webm, Some(F::Webm)), [Output::Webm, Output::Mkv]);
        assert_eq!(outputs_to_try(&mp4, Some(F::Webm)), [Output::Mp4, Output::Mkv], "H.264 never goes into WebM");
        assert_eq!(outputs_to_try(&mp4, Some(F::Mkv)), [Output::Mkv]);
        assert_eq!(outputs_to_try(&mp4, None), [Output::Mp4, Output::Mkv]);
        assert_eq!(outputs_to_try(&webm, None), [Output::Mkv]);
        let s = strings(&mux_args(Some(webm[0]), None, &abs("o.webm"), Output::Webm).unwrap());
        assert!(s.windows(2).any(|w| w == ["-f", "webm"]));
    }

    #[test]
    fn a_missing_program_is_a_clear_error() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let r = rt.block_on(run_process(&abs("no-such-ffmpeg"), &[], Duration::from_secs(5), &CancellationToken::new()));
        assert!(matches!(r, Err(MediaError::Remux(m)) if m.contains("could not start")));
    }

    fn sleeper() -> (PathBuf, Vec<OsString>) {
        if cfg!(windows) {
            (PathBuf::from(r"C:\Windows\System32\ping.exe"), ["-n", "30", "127.0.0.1"].map(OsString::from).to_vec())
        } else {
            (PathBuf::from("/bin/sleep"), vec![OsString::from("30")])
        }
    }

    #[test]
    fn a_stuck_process_is_killed_at_the_timeout_and_on_cancel() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (prog, args) = sleeper();
        if !prog.exists() {
            return; // unusual system: nothing to run
        }
        let t = std::time::Instant::now();
        let r = rt.block_on(run_process(&prog, &args, Duration::from_millis(300), &CancellationToken::new()));
        assert!(matches!(&r, Err(MediaError::Remux(m)) if m.contains("too long")), "{r:?}");
        assert!(t.elapsed() < Duration::from_secs(10));

        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        let t = std::time::Instant::now();
        let r = rt.block_on(async {
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                c2.cancel();
            });
            run_process(&prog, &args, Duration::from_secs(60), &cancel).await
        });
        assert!(matches!(r, Err(MediaError::Cancelled)), "{r:?}");
        assert!(t.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn finding_ffmpeg_ignores_relative_path_entries() {
        // An empty or relative PATH entry must never make the current directory a search location.
        let dir = std::env::temp_dir().join(format!("velox-ffmpeg-find-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
        assert!(find_ffmpeg(Some(&dir)).is_none_or(|p| !p.starts_with(&dir)), "no file yet");
        std::fs::write(dir.join(name), b"x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(find_ffmpeg(Some(&dir)).is_none_or(|p| !p.starts_with(&dir)), "not executable");
            std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(find_ffmpeg(Some(&dir)), Some(dir.join(name)), "a copy next to the app wins");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
