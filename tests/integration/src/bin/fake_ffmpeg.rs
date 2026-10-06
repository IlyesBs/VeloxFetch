//! A stand-in for ffmpeg used by the media tests: it "muxes" by concatenating its inputs, and
//! records the arguments it was given. It exists so the tests run on machines without ffmpeg.
//!
//! `FAKE_FFMPEG_LOG` = file to append the arguments to; `FAKE_FFMPEG_FAIL` = exit with an error.

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Ok(log) = std::env::var("FAKE_FFMPEG_LOG") {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
            let _ = writeln!(f, "{}", args.join("\u{1f}"));
        }
    }
    if std::env::var("FAKE_FFMPEG_FAIL").is_ok() {
        eprintln!("Invalid data found when processing input");
        std::process::exit(1);
    }
    let mut out = b"FAKEMUX\n".to_vec();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-i" {
            if let Some(p) = args.get(i + 1) {
                out.extend(std::fs::read(p).unwrap_or_default());
            }
            i += 1;
        }
        i += 1;
    }
    let target = args.last().expect("an output path");
    std::fs::write(target, out).expect("write the output");
}
