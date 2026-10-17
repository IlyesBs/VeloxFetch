//! A stand-in for yt-dlp used by the video-page tests, so they run on machines without it and never
//! touch the internet. It behaves according to the page link (always its last argument):
//! * `...drm...`  -> fails the way yt-dlp does for a DRM-protected video;
//! * `...slow...` -> writes the start of a video part, then downloads forever (until it is killed);
//! * `...jshang...` -> hangs whenever it is given a JavaScript runtime (`--js-runtimes`), like Deno on some
//!   computers; works with `--no-js-runtimes`;
//! * `...flaky...` -> the first download in a work folder fails like a refused request, the next one works;
//! * `.../playlist?list=<id>` -> lists a playlist (`--flat-playlist`): videos `watch?v=<id>-1`.. (5 when the id has
//!   `slow`, else 4), a private one and a link to another host; titles are very long when the id has `long`;
//! * anything else -> describes a video (`--dump-single-json`) or "downloads" it into `--paths`.
//!
//! Every call appends its arguments (separated by U+001F) to `velox-fake-ytdlp.log` in the temporary
//! folder; the tests find their own calls by the unique video id in the link.

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let log = std::env::temp_dir().join("velox-fake-ytdlp.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        // One write per call, so lines from runs at the same time never mix.
        let _ = f.write_all(format!("{}\n", args.join("\u{1f}")).as_bytes());
    }
    let url = args.last().cloned().unwrap_or_default();
    if url.contains("drm") {
        eprintln!("WARNING: something\nERROR: [generic] abc: This video is DRM protected");
        std::process::exit(1);
    }
    if url.contains("jshang") && args.iter().any(|a| a == "--js-runtimes") {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
    if args.iter().any(|a| a == "--dump-single-json") && url.contains("/playlist?list=") {
        let list = url.split("list=").nth(1).unwrap_or_default();
        let n = if list.contains("slow") { 5 } else { 4 };
        let mut entries: Vec<String> = (1..=n)
            .map(|i| {
                let title = if list.contains("long") {
                    format!("Clip {i}: a very long title that goes on and on, the way real ones do on YouTube | Channel name Ep. {i}")
                } else {
                    format!("Clip {i}: part/{i}")
                };
                format!(r#"{{"_type":"url","ie_key":"Youtube","url":"https://www.youtube.com/watch?v={list}-{i}","title":"{title}","duration":{}}}"#, i * 10)
            })
            .collect();
        entries.push(format!(r#"{{"_type":"url","url":"https://www.youtube.com/watch?v={list}-p","title":"[Private video]"}}"#));
        entries.push(format!(r#"{{"_type":"url","url":"http://127.0.0.1/{list}","title":"local"}}"#));
        println!(r#"{{"_type":"playlist","title":"My list: best/of","playlist_count":{},"entries":[{}]}}"#, n + 2, entries.join(","));
        return;
    }
    if args.iter().any(|a| a == "--dump-single-json") {
        println!(
            "{}",
            r#"{"title":"Fake clip: test/1","duration":61.0,"formats":[
                {"format_id":"140","vcodec":"none","acodec":"mp4a.40.2","filesize":1000000},
                {"format_id":"18","vcodec":"avc1.42001E","acodec":"mp4a.40.2","height":360,"width":640,"filesize":3000000},
                {"format_id":"136","vcodec":"avc1.4d401f","acodec":"none","height":720,"width":1280,"filesize":8000000},
                {"format_id":"137","vcodec":"avc1.640028","acodec":"none","height":1080,"width":1920,"filesize":20000000}]}"#
                .replace('\n', "")
        );
        return;
    }
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned().unwrap_or_default();
    let dir = std::path::PathBuf::from(value("--paths"));
    let selector = value("-f");
    let mut out = std::io::stdout();
    for done in [250u64, 500, 1000] {
        let _ = writeln!(out, "VELOXP {done} 1000 NA");
        let _ = out.flush();
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    if url.contains("flaky") && !dir.join("flaky.marker").exists() {
        std::fs::write(dir.join("flaky.marker"), b"1").expect("write the marker");
        eprintln!("ERROR: [youtube] abc: HTTP Error 403: Forbidden");
        std::process::exit(1);
    }
    if url.contains("slow") {
        // The picture part, partly written (a player may watch it while it grows).
        std::fs::write(dir.join("media.f137.mp4.part"), vec![7u8; 300 * 1024]).expect("write a part");
        loop {
            let _ = writeln!(out, "VELOXP 1000 1000000 NA");
            let _ = out.flush();
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    // An intermediate file yt-dlp would have removed must not be what gets published.
    std::fs::write(dir.join("media.f137.mp4.part"), b"partial").expect("write a part file");
    std::fs::remove_file(dir.join("media.f137.mp4.part")).ok();
    // The file type, as yt-dlp would make it: the extracted sound's type, else the joined type, else MP4.
    let ext = if args.iter().any(|a| a == "--extract-audio") {
        value("--audio-format")
    } else {
        value("--merge-output-format").split('/').next().filter(|e| !e.is_empty()).unwrap_or("mp4").to_string()
    };
    if args.iter().any(|a| a == "--ffmpeg-location") {
        let _ = writeln!(out, "[Merger] Merging formats into \"media.{ext}\"");
        let _ = out.flush();
    }
    std::fs::write(dir.join(format!("media.{ext}")), format!("FAKEVIDEO {selector}")).expect("write the video");
}
