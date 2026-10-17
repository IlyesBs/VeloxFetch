//! HLS: master playlists (qualities, audio and subtitle renditions) and media playlists
//! (segments, initialization segment, byte ranges, discontinuities).
//!
//! Only what can be downloaded faithfully is accepted. A stream that is encrypted, live, or that
//! changes its initialization segment half way is refused with a clear reason.

use url::Url;

use crate::{
    byte_range, limits, quality_label, resolve_url, title_from_url, Analysis, Catalog, Container, Entry, MediaError, Result, Segment,
    Source, StreamKind, StreamOption, TrackOption, TrackPlan,
};

/// `KEY=value,KEY="quoted, value"` (the attribute list format of HLS tags).
pub(crate) fn attributes(list: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let b = list.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i] == b',' || b[i] == b' ') {
            i += 1;
        }
        let ks = i;
        while i < b.len() && b[i] != b'=' && b[i] != b',' {
            i += 1;
        }
        if i >= b.len() || b[i] != b'=' {
            continue;
        }
        let key = list[ks..i].trim().to_ascii_uppercase();
        i += 1;
        let value = if i < b.len() && b[i] == b'"' {
            i += 1;
            let vs = i;
            while i < b.len() && b[i] != b'"' {
                i += 1;
            }
            let v = &list[vs..i];
            i = (i + 1).min(b.len());
            v
        } else {
            let vs = i;
            while i < b.len() && b[i] != b',' {
                i += 1;
            }
            &list[vs..i]
        };
        out.push((key, value.trim().to_string()));
    }
    out
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

/// `n` or `n@o`.
fn parse_range(spec: &str, previous_end: Option<u64>) -> Result<(u64, u64)> {
    let (len, off) = match spec.split_once('@') {
        Some((l, o)) => (l, Some(o)),
        None => (spec, None),
    };
    let len: u64 = len.trim().parse().map_err(|_| MediaError::invalid("a byte range is not a number"))?;
    let off = match off {
        Some(o) => o.trim().parse().map_err(|_| MediaError::invalid("a byte range offset is not a number"))?,
        None => previous_end.map(|e| e + 1).unwrap_or(0),
    };
    byte_range(len, off)
}

pub(crate) enum Playlist {
    Master(Master),
    Media(TrackPlan),
}

pub(crate) struct Master {
    streams: Vec<HlsStream>,
    renditions: Vec<Rendition>,
}

struct HlsStream {
    uri: Url,
    bandwidth: Option<u64>,
    resolution: Option<(u32, u32)>,
    codecs: Option<String>,
    audio: Option<String>,
}

#[derive(PartialEq)]
enum RenditionKind {
    Audio,
    Subtitles,
}

struct Rendition {
    kind: RenditionKind,
    group: String,
    name: String,
    language: Option<String>,
    /// No URI = the audio is inside the variant's own segments.
    uri: Option<Url>,
    default: bool,
}

pub(crate) fn parse(base: &Url, text: &str) -> Result<Playlist> {
    if text.len() > limits::MAX_MANIFEST_BYTES {
        return Err(MediaError::limit("the playlist is larger than 4 MiB"));
    }
    let text = strip_bom(text);
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    if lines.next() != Some("#EXTM3U") {
        return Err(MediaError::invalid("this is not an HLS playlist (it does not start with #EXTM3U)"));
    }
    if text.contains("#EXT-X-STREAM-INF") {
        parse_master(base, text).map(Playlist::Master)
    } else {
        parse_media(base, text).map(Playlist::Media)
    }
}

fn parse_master(base: &Url, text: &str) -> Result<Master> {
    let mut streams = Vec::new();
    let mut renditions = Vec::new();
    let mut pending: Option<Vec<(String, String)>> = None;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            pending = Some(attributes(rest));
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA:") {
            let a = attributes(rest);
            let kind = match attr(&a, "TYPE") {
                Some("AUDIO") => RenditionKind::Audio,
                Some("SUBTITLES") => RenditionKind::Subtitles,
                _ => continue, // VIDEO renditions (angles) and CLOSED-CAPTIONS are not downloadable tracks
            };
            let uri = match attr(&a, "URI") {
                Some(u) if !u.is_empty() => Some(resolve_url(base, u)?),
                _ => None,
            };
            if kind == RenditionKind::Subtitles && uri.is_none() {
                continue;
            }
            renditions.push(Rendition {
                kind,
                group: attr(&a, "GROUP-ID").unwrap_or("").to_string(),
                name: attr(&a, "NAME").unwrap_or("").to_string(),
                language: attr(&a, "LANGUAGE").map(str::to_string),
                uri,
                default: attr(&a, "DEFAULT") == Some("YES"),
            });
        } else if !line.starts_with('#') {
            if let Some(a) = pending.take() {
                let resolution = attr(&a, "RESOLUTION").and_then(|r| {
                    let (w, h) = r.split_once(['x', 'X'])?;
                    Some((w.parse().ok()?, h.parse().ok()?))
                });
                streams.push(HlsStream {
                    uri: resolve_url(base, line)?,
                    bandwidth: attr(&a, "BANDWIDTH").and_then(|b| b.parse().ok()),
                    resolution,
                    codecs: attr(&a, "CODECS").map(str::to_string),
                    audio: attr(&a, "AUDIO").map(str::to_string),
                });
            }
        }
        if streams.len() + renditions.len() > limits::MAX_TRACKS {
            return Err(MediaError::limit(format!("more than {} tracks in one playlist", limits::MAX_TRACKS)));
        }
    }
    if streams.is_empty() {
        return Err(MediaError::invalid("the playlist lists no streams"));
    }
    Ok(Master { streams, renditions })
}

fn container_of(url: &Url, has_init: bool) -> Container {
    let ext = url.path().rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "m4s" | "mp4" | "m4a" | "m4v" | "cmfv" | "cmfa" => Container::Fmp4,
        "aac" => Container::Aac,
        "mp3" => Container::Mp3,
        "vtt" | "webvtt" => Container::Vtt,
        "webm" => Container::Webm,
        _ if has_init => Container::Fmp4,
        _ => Container::Ts,
    }
}

fn parse_media(base: &Url, text: &str) -> Result<TrackPlan> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut init: Option<Segment> = None;
    let mut duration = 0.0f64;
    let mut pending_len: Option<f64> = None;
    let mut pending_range: Option<String> = None;
    let mut last_range: Option<(Url, u64)> = None;
    let mut discontinuities = 0u32;
    let (mut endlist, mut vod) = (false, false);

    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let d: f64 = rest.split(',').next().unwrap_or("").trim().parse().unwrap_or(0.0);
            pending_len = Some(if d.is_finite() && d > 0.0 { d } else { 0.0 });
        } else if let Some(rest) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            pending_range = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            let a = attributes(rest);
            let method = attr(&a, "METHOD").unwrap_or("NONE");
            if method != "NONE" {
                return Err(MediaError::unsupported(format!(
                    "this stream is encrypted ({method}). Encrypted and DRM-protected streams are not supported"
                )));
            }
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            let a = attributes(rest);
            let uri = attr(&a, "URI").ok_or_else(|| MediaError::invalid("an initialization segment has no URI"))?;
            let url = resolve_url(base, uri)?;
            let range = attr(&a, "BYTERANGE").map(|r| parse_range(r, None)).transpose()?;
            let seg = Segment { url, range };
            match &init {
                None if segments.is_empty() => init = Some(seg),
                Some(existing) if *existing == seg => {}
                _ => {
                    return Err(MediaError::unsupported(
                        "the stream changes its initialization segment part-way through, which is not supported",
                    ))
                }
            }
        } else if line == "#EXT-X-DISCONTINUITY" {
            discontinuities = discontinuities.saturating_add(1);
        } else if line == "#EXT-X-ENDLIST" {
            endlist = true;
        } else if let Some(rest) = line.strip_prefix("#EXT-X-PLAYLIST-TYPE:") {
            vod = rest.trim() == "VOD";
        } else if !line.starts_with('#') {
            let url = resolve_url(base, line)?;
            let range = match pending_range.take() {
                Some(spec) => {
                    let prev = last_range.as_ref().filter(|(u, _)| *u == url).map(|(_, e)| *e);
                    let r = parse_range(&spec, prev)?;
                    last_range = Some((url.clone(), r.1));
                    Some(r)
                }
                None => None,
            };
            duration += pending_len.take().unwrap_or(0.0);
            segments.push(Segment { url, range });
            if segments.len() > limits::MAX_SEGMENTS {
                return Err(MediaError::limit(format!("more than {} segments in one track", limits::MAX_SEGMENTS)));
            }
        }
    }
    if !endlist && !vod {
        return Err(MediaError::unsupported("this is a live stream (it has no end), which is not supported"));
    }
    if segments.is_empty() {
        return Err(MediaError::invalid("the playlist has no segments"));
    }
    let container = container_of(&segments[0].url, init.is_some());
    Ok(TrackPlan { init, segments, container, duration, discontinuities })
}

/// Turn a parsed master playlist into the choices shown to the user.
pub(crate) fn catalog_from_master(base: &Url, m: Master) -> Catalog {
    let mut streams: Vec<(HlsStream, u64)> = m
        .streams
        .into_iter()
        .map(|s| {
            let b = s.bandwidth.unwrap_or(0);
            (s, b)
        })
        .collect();
    streams.sort_by_key(|s| std::cmp::Reverse(s.1));
    let entries: Vec<Entry<StreamOption>> = streams
        .into_iter()
        .enumerate()
        .map(|(i, (s, _))| Entry {
            option: StreamOption {
                id: format!("v{i}"),
                label: quality_label(s.resolution.map(|r| r.0), s.resolution.map(|r| r.1), s.bandwidth),
                bandwidth: s.bandwidth,
                width: s.resolution.map(|r| r.0),
                height: s.resolution.map(|r| r.1),
                codecs: s.codecs,
                group: s.audio,
            },
            source: Source::Playlist(s.uri),
        })
        .collect();

    let mut audio = Vec::new();
    let mut subtitles = Vec::new();
    for r in m.renditions {
        match (r.kind, r.uri) {
            (RenditionKind::Audio, Some(uri)) => audio.push(Entry {
                option: TrackOption {
                    id: format!("a{}", audio.len()),
                    label: label_of(&r.name, r.language.as_deref()),
                    language: r.language,
                    group: Some(r.group),
                    default: r.default,
                },
                source: Source::Playlist(uri),
            }),
            (RenditionKind::Subtitles, Some(uri)) => subtitles.push(Entry {
                option: TrackOption {
                    id: format!("s{}", subtitles.len()),
                    label: label_of(&r.name, r.language.as_deref()),
                    language: r.language,
                    group: Some(r.group),
                    default: r.default,
                },
                source: Source::Playlist(uri),
            }),
            _ => {}
        }
    }
    let analysis = Analysis {
        playlist: None,
        part_of_playlist: false,
        kind: StreamKind::Hls,
        title: title_from_url(base),
        duration_secs: None,
        streams: entries.iter().map(|e| e.option.clone()).collect(),
        audio: audio.iter().map(|e: &Entry<TrackOption>| e.option.clone()).collect(),
        subtitles: subtitles.iter().map(|e: &Entry<TrackOption>| e.option.clone()).collect(),
    };
    Catalog { analysis, streams: entries, audio, subtitles }
}

fn label_of(name: &str, language: Option<&str>) -> String {
    match (name.is_empty(), language) {
        (false, Some(l)) if !name.eq_ignore_ascii_case(l) => format!("{name} ({l})"),
        (false, _) => name.to_string(),
        (true, Some(l)) => l.to_string(),
        (true, None) => "Track".to_string(),
    }
}

/// A media playlist opened directly (no master): one quality, no separate tracks.
pub(crate) fn catalog_from_media(base: &Url, plan: TrackPlan) -> Catalog {
    let option =
        StreamOption { id: "v0".into(), label: "Video".into(), bandwidth: None, width: None, height: None, codecs: None, group: None };
    let analysis = Analysis {
        playlist: None,
        part_of_playlist: false,
        kind: StreamKind::Hls,
        title: title_from_url(base),
        duration_secs: (plan.duration > 0.0).then_some(plan.duration),
        streams: vec![option.clone()],
        audio: Vec::new(),
        subtitles: Vec::new(),
    };
    Catalog { analysis, streams: vec![Entry { option, source: Source::Ready(plan) }], audio: Vec::new(), subtitles: Vec::new() }
}

/// A variant / rendition playlist fetched after the user chose it.
pub(crate) fn parse_track(base: &Url, text: &str) -> Result<TrackPlan> {
    match parse(base, text)? {
        Playlist::Media(p) => Ok(p),
        Playlist::Master(_) => Err(MediaError::invalid("expected a media playlist but found another master playlist")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://cdn.example.com/v/master.m3u8").unwrap()
    }

    const MASTER: &str = "#EXTM3U\n#EXT-X-VERSION:6\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,URI=\"audio_en.m3u8\"\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Francais\",LANGUAGE=\"fr\",URI=\"audio_fr.m3u8\"\n\
#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",LANGUAGE=\"en\",URI=\"subs_en.m3u8\"\n\
#EXT-X-MEDIA:TYPE=CLOSED-CAPTIONS,GROUP-ID=\"cc\",NAME=\"CC\",INSTREAM-ID=\"CC1\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,CODECS=\"avc1.4d401e,mp4a.40.2\",AUDIO=\"aud\",SUBTITLES=\"subs\"\n360p.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=2800000,RESOLUTION=1280x720,CODECS=\"avc1.4d401f,mp4a.40.2\",AUDIO=\"aud\"\n720p.m3u8\n\
#EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=100000,URI=\"iframes.m3u8\"\n";

    #[test]
    fn attribute_lists_handle_quoted_commas() {
        let a = attributes("BANDWIDTH=800000,CODECS=\"avc1.4d401e,mp4a.40.2\",RESOLUTION=640x360");
        assert_eq!(attr(&a, "CODECS"), Some("avc1.4d401e,mp4a.40.2"));
        assert_eq!(attr(&a, "RESOLUTION"), Some("640x360"));
        assert_eq!(attr(&a, "BANDWIDTH"), Some("800000"));
        assert!(attributes("garbage").is_empty());
        assert!(attributes("A=\"unterminated").len() <= 1);
        assert!(attributes("").is_empty());
    }

    #[test]
    fn a_master_playlist_lists_qualities_best_first_with_tracks() {
        let Playlist::Master(m) = parse(&base(), MASTER).unwrap() else { panic!("expected a master playlist") };
        let c = catalog_from_master(&base(), m);
        let labels: Vec<_> = c.analysis.streams.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["720p · 2.8 Mbit/s", "360p · 800 kbit/s"], "best first; the I-frame playlist is ignored");
        assert_eq!(c.analysis.streams[0].group.as_deref(), Some("aud"));
        assert_eq!(c.analysis.audio.len(), 2);
        assert!(c.analysis.audio[0].default && !c.analysis.audio[1].default);
        assert_eq!(c.analysis.subtitles.len(), 1, "closed captions are not tracks");
        match &c.streams[0].source {
            Source::Playlist(u) => assert_eq!(u.as_str(), "https://cdn.example.com/v/720p.m3u8"),
            _ => panic!(),
        }
    }

    const MEDIA: &str = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:6\n#EXT-X-PLAYLIST-TYPE:VOD\n\
#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:6.0,\nseg0.m4s\n#EXTINF:6.0,\nseg1.m4s\n#EXT-X-DISCONTINUITY\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4.5,\nseg2.m4s\n#EXT-X-ENDLIST\n";

    #[test]
    fn a_media_playlist_gives_init_segments_duration_and_discontinuities() {
        let p = parse_track(&base(), MEDIA).unwrap();
        assert_eq!(p.segments.len(), 3);
        assert_eq!(p.init.as_ref().unwrap().url.as_str(), "https://cdn.example.com/v/init.mp4");
        assert!((p.duration - 16.5).abs() < 1e-9);
        assert_eq!(p.discontinuities, 1);
        assert_eq!(p.container, Container::Fmp4);
    }

    #[test]
    fn plain_ts_playlists_are_ts() {
        let p = parse_track(&base(), "#EXTM3U\n#EXTINF:5,\na.ts\n#EXTINF:5,\nb.ts\n#EXT-X-ENDLIST\n").unwrap();
        assert_eq!(p.container, Container::Ts);
        assert!(p.init.is_none());
    }

    #[test]
    fn byte_ranges_follow_each_other() {
        let t = "#EXTM3U\n#EXT-X-MAP:URI=\"f.mp4\",BYTERANGE=\"720@0\"\n#EXTINF:4,\n#EXT-X-BYTERANGE:1000@720\nf.mp4\n#EXTINF:4,\n#EXT-X-BYTERANGE:500\nf.mp4\n#EXT-X-ENDLIST\n";
        let p = parse_track(&base(), t).unwrap();
        assert_eq!(p.init.as_ref().unwrap().range, Some((0, 719)));
        assert_eq!(p.segments[0].range, Some((720, 1719)));
        assert_eq!(p.segments[1].range, Some((1720, 2219)));
    }

    #[test]
    fn unsupported_streams_are_refused_with_a_reason() {
        let encrypted = "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k.bin\"\n#EXTINF:5,\na.ts\n#EXT-X-ENDLIST\n";
        assert!(matches!(parse_track(&base(), encrypted), Err(MediaError::Unsupported(m)) if m.contains("encrypted")));
        let sample = "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\"\n#EXTINF:5,\na.ts\n#EXT-X-ENDLIST\n";
        assert!(matches!(parse_track(&base(), sample), Err(MediaError::Unsupported(_))));
        let clear = "#EXTM3U\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:5,\na.ts\n#EXT-X-ENDLIST\n";
        assert!(parse_track(&base(), clear).is_ok(), "METHOD=NONE is not encryption");
        let live = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:5,\na.ts\n";
        assert!(matches!(parse_track(&base(), live), Err(MediaError::Unsupported(m)) if m.contains("live")));
        let changing =
            "#EXTM3U\n#EXT-X-MAP:URI=\"i1.mp4\"\n#EXTINF:5,\na.m4s\n#EXT-X-MAP:URI=\"i2.mp4\"\n#EXTINF:5,\nb.m4s\n#EXT-X-ENDLIST\n";
        assert!(matches!(parse_track(&base(), changing), Err(MediaError::Unsupported(_))));
    }

    #[test]
    fn hostile_playlists_are_refused_not_followed() {
        for uri in ["file:///etc/passwd", "http://insecure.example/a.ts", "https://u:p@h/a.ts", "ftp://h/a.ts", "gopher://h/x"] {
            let t = format!("#EXTM3U\n#EXTINF:5,\n{uri}\n#EXT-X-ENDLIST\n");
            assert!(parse_track(&base(), &t).is_err(), "{uri}");
        }
        assert!(parse(&base(), "<html>not a playlist</html>").is_err());
        assert!(parse(&base(), "").is_err());
        assert!(parse_track(&base(), "#EXTM3U\n#EXT-X-ENDLIST\n").is_err(), "no segments");
        assert!(parse_track(&base(), "#EXTM3U\n#EXTINF:5,\na.ts\n#EXT-X-BYTERANGE:x@y\nb.ts\n#EXT-X-ENDLIST").is_err());
        // Byte range whose end overflows u64.
        let t = format!("#EXTM3U\n#EXTINF:5,\n#EXT-X-BYTERANGE:100@{}\na.ts\n#EXT-X-ENDLIST\n", u64::MAX - 5);
        assert!(parse_track(&base(), &t).is_err());
    }

    #[test]
    fn size_and_count_limits_hold() {
        let mut t = String::from("#EXTM3U\n");
        for i in 0..=limits::MAX_SEGMENTS {
            t.push_str(&format!("#EXTINF:1,\ns{i}.ts\n"));
        }
        t.push_str("#EXT-X-ENDLIST\n");
        assert!(matches!(parse_track(&base(), &t), Err(MediaError::Limit(_))));
        let huge = format!("#EXTM3U\n#{}\n", "x".repeat(limits::MAX_MANIFEST_BYTES));
        assert!(matches!(parse(&base(), &huge), Err(MediaError::Limit(_))));
        let mut m = String::from("#EXTM3U\n");
        for i in 0..=limits::MAX_TRACKS {
            m.push_str(&format!("#EXT-X-STREAM-INF:BANDWIDTH={}\nv{i}.m3u8\n", i + 1));
        }
        assert!(matches!(parse(&base(), &m), Err(MediaError::Limit(_))));
    }

    #[test]
    fn a_media_playlist_can_be_opened_directly() {
        let Playlist::Media(p) = parse(&base(), MEDIA).unwrap() else { panic!() };
        let c = catalog_from_media(&base(), p);
        assert_eq!(c.analysis.streams.len(), 1);
        assert_eq!(c.analysis.duration_secs, Some(16.5));
        assert_eq!(c.analysis.title, "master");
    }

    #[test]
    fn windows_line_endings_and_a_bom_are_fine() {
        let t = "\u{feff}#EXTM3U\r\n#EXTINF:5,\r\na.ts\r\n#EXT-X-ENDLIST\r\n";
        assert_eq!(parse_track(&base(), t).unwrap().segments.len(), 1);
    }
}
