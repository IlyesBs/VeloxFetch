//! DASH: MPD manifests (AdaptationSet, Representation, initialization and media segments).
//!
//! The XML is parsed into a small bounded tree first (depth and node limits, DTDs refused), then
//! interpreted. Supported: one static Period; `SegmentTemplate` (`$Number$` / `$Time$`, with or
//! without a `SegmentTimeline`), `SegmentList`, and single-file representations (`BaseURL` /
//! `SegmentBase`). Refused with a reason: live (dynamic) streams, several periods, DRM.

use quick_xml::events::Event;
use quick_xml::Reader;
use url::Url;

use crate::{
    limits, quality_label, resolve_url, title_from_url, Analysis, Catalog, Container, Entry, MediaError, Result, Segment, Source,
    StreamKind, StreamOption, TrackOption, TrackPlan,
};

#[derive(Default, Debug)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
    text: String,
}

impl Node {
    fn attr(&self, key: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
    fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }
    fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> {
        self.children.iter().filter(move |c| c.name == name)
    }
}

fn local(name: &str) -> String {
    name.rsplit(':').next().unwrap_or("").to_string()
}

fn parse_xml(text: &str) -> Result<Node> {
    if text.len() > limits::MAX_MANIFEST_BYTES {
        return Err(MediaError::limit("the manifest is larger than 4 MiB"));
    }
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<Node> = Vec::new();
    let mut root: Option<Node> = None;
    let mut nodes = 0usize;

    let make = |e: &quick_xml::events::BytesStart| -> Result<Node> {
        let mut attrs = Vec::new();
        for a in e.attributes() {
            let a = a.map_err(|_| MediaError::invalid("the manifest has a malformed attribute"))?;
            #[allow(deprecated)] // the replacement needs an XML version we do not track; entities stay off either way
            let value = a.unescape_value().map_err(|_| MediaError::invalid("the manifest has a malformed value"))?;
            attrs.push((local(a.key.as_ref()), value.into_owned()));
        }
        Ok(Node { name: local(e.name().as_ref()), attrs, ..Default::default() })
    };

    loop {
        let event = reader.read_event().map_err(|_| MediaError::invalid("the manifest is not well-formed XML"))?;
        match event {
            Event::Start(e) => {
                nodes += 1;
                if nodes > limits::MAX_XML_NODES {
                    return Err(MediaError::limit("the manifest has too many elements"));
                }
                if stack.len() >= limits::MAX_XML_DEPTH {
                    return Err(MediaError::limit("the manifest is nested too deeply"));
                }
                stack.push(make(&e)?);
            }
            Event::Empty(e) => {
                nodes += 1;
                if nodes > limits::MAX_XML_NODES {
                    return Err(MediaError::limit("the manifest has too many elements"));
                }
                let n = make(&e)?;
                match stack.last_mut() {
                    Some(p) => p.children.push(n),
                    None => root = Some(n),
                }
            }
            Event::End(_) => {
                let n = stack.pop().ok_or_else(|| MediaError::invalid("the manifest is not well-formed XML"))?;
                match stack.last_mut() {
                    Some(p) => p.children.push(n),
                    None => root = Some(n),
                }
            }
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    if top.text.len() < limits::MAX_URL_LEN {
                        top.text.push_str(&t.xml10_content());
                    }
                }
            }
            Event::DocType(_) => return Err(MediaError::unsupported("the manifest declares a DTD, which is not allowed")),
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(MediaError::invalid("the manifest ends unexpectedly"));
    }
    root.ok_or_else(|| MediaError::invalid("the manifest is empty"))
}

/// `PT1H2M3.5S`, `P0DT30S`... to seconds.
pub(crate) fn iso_duration(s: &str) -> Option<f64> {
    let s = s.trim().strip_prefix('P')?;
    let (date, time) = s.split_once('T').map_or((s, ""), |(d, t)| (d, t));
    let mut total = 0.0f64;
    for (part, units) in [
        (date, &[('Y', 31_536_000.0), ('M', 2_592_000.0), ('W', 604_800.0), ('D', 86_400.0)][..]),
        (time, &[('H', 3600.0), ('M', 60.0), ('S', 1.0)][..]),
    ] {
        let mut num = String::new();
        for c in part.chars() {
            if c.is_ascii_digit() || c == '.' {
                num.push(c);
            } else {
                let mult = units.iter().find(|(u, _)| *u == c)?.1;
                total += num.parse::<f64>().ok()? * mult;
                num.clear();
            }
        }
        if !num.is_empty() {
            return None;
        }
    }
    (total.is_finite() && total >= 0.0).then_some(total)
}

/// `$RepresentationID$`, `$Number%05d$`, `$Bandwidth$`, `$Time$`, `$$`.
fn expand(template: &str, id: &str, bandwidth: u64, number: Option<u64>, time: Option<u64>) -> Result<String> {
    let mut out = String::with_capacity(template.len() + 16);
    let mut parts = template.split('$');
    out.push_str(parts.next().unwrap_or(""));
    let mut open = true;
    let mut rest: Vec<&str> = parts.collect();
    rest.reverse();
    while let Some(token) = rest.pop() {
        if open {
            if token.is_empty() {
                out.push('$');
            } else {
                let (name, fmt) = token.split_once('%').map_or((token, None), |(n, f)| (n, Some(f)));
                let width = match fmt {
                    None => 0,
                    Some(f) => {
                        let f = f.strip_suffix('d').ok_or_else(|| MediaError::invalid("a segment template has an unsupported format"))?;
                        let w = f.strip_prefix('0').unwrap_or(f);
                        let w: usize = if w.is_empty() {
                            0
                        } else {
                            w.parse().map_err(|_| MediaError::invalid("a segment template has a bad width"))?
                        };
                        if w > 20 {
                            return Err(MediaError::invalid("a segment template has a bad width"));
                        }
                        w
                    }
                };
                let num =
                    |v: Option<u64>| v.ok_or_else(|| MediaError::invalid("a segment template uses a value that is not available here"));
                match name {
                    "RepresentationID" => out.push_str(id),
                    "Number" => out.push_str(&format!("{:0width$}", num(number)?)),
                    "Bandwidth" => out.push_str(&format!("{bandwidth:0width$}")),
                    "Time" => out.push_str(&format!("{:0width$}", num(time)?)),
                    _ => return Err(MediaError::invalid("a segment template uses an unknown variable")),
                }
            }
            open = false;
        } else {
            out.push_str(token);
            open = true;
        }
    }
    if !open {
        return Err(MediaError::invalid("a segment template has an unmatched `$`"));
    }
    Ok(out)
}

/// Base URLs stack: the first `<BaseURL>` of each level, resolved against the previous.
fn with_base(base: &Url, node: &Node) -> Result<Url> {
    match node.child("BaseURL") {
        Some(b) if !b.text.trim().is_empty() => resolve_url(base, &b.text),
        _ => Ok(base.clone()),
    }
}

fn has_protection(n: &Node) -> bool {
    n.children.iter().any(|c| c.name == "ContentProtection" || has_protection(c))
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Video,
    Audio,
    Text,
}

fn kind_of(set: &Node, rep: &Node) -> Option<Kind> {
    let ct = set.attr("contentType");
    let mime = rep.attr("mimeType").or(set.attr("mimeType")).unwrap_or("");
    let codecs = rep.attr("codecs").or(set.attr("codecs")).unwrap_or("");
    if ct == Some("video") || mime.starts_with("video/") {
        Some(Kind::Video)
    } else if ct == Some("audio") || mime.starts_with("audio/") {
        Some(Kind::Audio)
    } else if ct == Some("text") || mime.starts_with("text/") || mime.contains("ttml") || mime.contains("vtt") || codecs == "wvtt" {
        Some(Kind::Text)
    } else {
        None
    }
}

fn container_for(kind: Kind, mime: &str, url: &Url) -> Result<Container> {
    let m = mime.to_ascii_lowercase();
    if m.contains("webm") {
        return Ok(Container::Webm);
    }
    if kind == Kind::Text {
        if m.contains("ttml") || url.path().ends_with(".ttml") || url.path().ends_with(".xml") {
            return Ok(Container::Ttml);
        }
        if m.contains("vtt") || url.path().ends_with(".vtt") {
            return Ok(Container::Vtt);
        }
        return Err(MediaError::unsupported("this subtitle format is not supported"));
    }
    if m.contains("mp4") || m.is_empty() {
        return Ok(Container::Fmp4);
    }
    if m.contains("mpeg") && kind == Kind::Audio {
        return Ok(Container::Mp3);
    }
    Err(MediaError::unsupported(format!("the media type `{mime}` is not supported")))
}

struct Ctx<'a> {
    base: Url,
    /// Period length in seconds (falls back to the manifest's).
    duration: Option<f64>,
    set: &'a Node,
}

/// Inherit an attribute from the Representation, then the AdaptationSet.
fn template_attr<'a>(rep_t: Option<&'a Node>, set_t: Option<&'a Node>, key: &str) -> Option<&'a str> {
    rep_t.and_then(|t| t.attr(key)).or_else(|| set_t.and_then(|t| t.attr(key)))
}

fn plan_for(ctx: &Ctx, rep: &Node, kind: Kind, id: &str, bandwidth: u64) -> Result<TrackPlan> {
    let base = with_base(&ctx.base, rep)?;
    let mime = rep.attr("mimeType").or(ctx.set.attr("mimeType")).unwrap_or("");
    let rep_t = rep.child("SegmentTemplate");
    let set_t = ctx.set.child("SegmentTemplate");

    let (init, segments): (Option<Segment>, Vec<Segment>) = if rep_t.is_some() || set_t.is_some() {
        let media = template_attr(rep_t, set_t, "media").ok_or_else(|| MediaError::invalid("a segment template has no media pattern"))?;
        let init = match template_attr(rep_t, set_t, "initialization") {
            Some(t) => Some(Segment { url: resolve_url(&base, &expand(t, id, bandwidth, None, None)?)?, range: None }),
            None => None,
        };
        let timescale: f64 =
            template_attr(rep_t, set_t, "timescale").and_then(|v| v.parse().ok()).filter(|t: &f64| *t > 0.0).unwrap_or(1.0);
        let start_number: u64 = template_attr(rep_t, set_t, "startNumber").and_then(|v| v.parse().ok()).unwrap_or(1);
        let timeline = rep_t.and_then(|t| t.child("SegmentTimeline")).or_else(|| set_t.and_then(|t| t.child("SegmentTimeline")));
        let uses_time = media.contains("$Time");
        let mut segs = Vec::new();
        let mut push = |number: u64, time: u64| -> Result<()> {
            if segs.len() >= limits::MAX_SEGMENTS {
                return Err(MediaError::limit(format!("more than {} segments in one track", limits::MAX_SEGMENTS)));
            }
            let expanded = expand(media, id, bandwidth, Some(number), Some(time))?;
            segs.push(Segment { url: resolve_url(&base, &expanded)?, range: None });
            Ok(())
        };
        if let Some(tl) = timeline {
            let mut time = 0u64;
            let mut number = start_number;
            let period_end = ctx.duration.map(|d| (d * timescale) as u64);
            for s in tl.all("S") {
                let d: u64 = s
                    .attr("d")
                    .and_then(|v| v.parse().ok())
                    .filter(|d| *d > 0)
                    .ok_or_else(|| MediaError::invalid("a timeline entry has no duration"))?;
                if let Some(t) = s.attr("t").and_then(|v| v.parse().ok()) {
                    time = t;
                }
                let r: i64 = s.attr("r").and_then(|v| v.parse().ok()).unwrap_or(0);
                let count = if r >= 0 {
                    r as u64 + 1
                } else {
                    let end = period_end.ok_or_else(|| MediaError::invalid("a repeating timeline needs the stream duration"))?;
                    end.saturating_sub(time).div_ceil(d)
                };
                for _ in 0..count {
                    push(number, time)?;
                    number += 1;
                    time = time.checked_add(d).ok_or_else(|| MediaError::invalid("a timeline is too long"))?;
                }
            }
        } else {
            let d: f64 = template_attr(rep_t, set_t, "duration")
                .and_then(|v| v.parse().ok())
                .filter(|d: &f64| *d > 0.0)
                .ok_or_else(|| MediaError::invalid("a segment template has no duration or timeline"))?;
            let total = ctx.duration.ok_or_else(|| MediaError::invalid("the manifest does not say how long the stream is"))?;
            let count = ((total * timescale) / d).ceil();
            if !count.is_finite() || count < 1.0 {
                return Err(MediaError::invalid("the stream has no segments"));
            }
            if count > limits::MAX_SEGMENTS as f64 {
                return Err(MediaError::limit(format!("more than {} segments in one track", limits::MAX_SEGMENTS)));
            }
            for i in 0..count as u64 {
                push(start_number + i, if uses_time { (i as f64 * d) as u64 } else { 0 })?;
            }
        }
        (init, segs)
    } else if let Some(list) = rep.child("SegmentList").or_else(|| ctx.set.child("SegmentList")) {
        let init = match list.child("Initialization") {
            Some(i) => {
                let src = i.attr("sourceURL").ok_or_else(|| MediaError::invalid("an initialization element has no source"))?;
                Some(Segment { url: resolve_url(&base, src)?, range: None })
            }
            None => None,
        };
        let mut segs = Vec::new();
        for s in list.all("SegmentURL") {
            if segs.len() >= limits::MAX_SEGMENTS {
                return Err(MediaError::limit(format!("more than {} segments in one track", limits::MAX_SEGMENTS)));
            }
            let media = s.attr("media").ok_or_else(|| MediaError::invalid("a segment has no address"))?;
            segs.push(Segment { url: resolve_url(&base, media)?, range: None });
        }
        (init, segs)
    } else {
        // One file holds the whole track (BaseURL, optionally with SegmentBase for indexing).
        let is_file = rep.child("BaseURL").is_some_and(|b| !b.text.trim().is_empty());
        if !is_file {
            return Err(MediaError::unsupported("a representation has no segments this app can locate"));
        }
        (None, vec![Segment { url: base.clone(), range: None }])
    };
    if segments.is_empty() {
        return Err(MediaError::invalid("a representation has no segments"));
    }
    let container = container_for(kind, mime, &segments[0].url)?;
    Ok(TrackPlan { init, segments, container, duration: ctx.duration.unwrap_or(0.0), discontinuities: 0 })
}

pub(crate) fn parse(base: &Url, text: &str) -> Result<Catalog> {
    let root = parse_xml(text)?;
    if root.name != "MPD" {
        return Err(MediaError::invalid("this is not a DASH manifest (no MPD element)"));
    }
    if root.attr("type") == Some("dynamic") {
        return Err(MediaError::unsupported("this is a live stream (it has no end), which is not supported"));
    }
    if has_protection(&root) {
        return Err(MediaError::unsupported("this stream is protected (DRM). Protected streams are not supported"));
    }
    let periods: Vec<&Node> = root.all("Period").collect();
    match periods.len() {
        0 => return Err(MediaError::invalid("the manifest has no Period")),
        1 => {}
        _ => return Err(MediaError::unsupported("this stream has several periods (often advertising breaks), which is not supported")),
    }
    let period = periods[0];
    let mpd_base = with_base(base, &root)?;
    let period_base = with_base(&mpd_base, period)?;
    let duration = period.attr("duration").or(root.attr("mediaPresentationDuration")).and_then(iso_duration).filter(|d| *d > 0.0);

    let mut streams: Vec<Entry<StreamOption>> = Vec::new();
    let mut audio: Vec<Entry<TrackOption>> = Vec::new();
    let mut subtitles: Vec<Entry<TrackOption>> = Vec::new();
    let mut total = 0usize;
    for set in period.all("AdaptationSet") {
        let set_base = with_base(&period_base, set)?;
        let lang = set.attr("lang").map(str::to_string);
        for rep in set.all("Representation") {
            total += 1;
            if total > limits::MAX_TRACKS {
                return Err(MediaError::limit(format!("more than {} tracks in one manifest", limits::MAX_TRACKS)));
            }
            let Some(kind) = kind_of(set, rep) else { continue };
            let id = rep.attr("id").unwrap_or("").to_string();
            let bandwidth: u64 = rep.attr("bandwidth").and_then(|b| b.parse().ok()).unwrap_or(0);
            let ctx = Ctx { base: set_base.clone(), duration, set };
            let plan = plan_for(&ctx, rep, kind, &id, bandwidth)?;
            let (width, height): (Option<u32>, Option<u32>) =
                (rep.attr("width").and_then(|w| w.parse().ok()), rep.attr("height").and_then(|h| h.parse().ok()));
            let codecs = rep.attr("codecs").or(set.attr("codecs")).map(str::to_string);
            match kind {
                Kind::Video => {
                    let n = streams.len();
                    streams.push(Entry {
                        option: StreamOption {
                            id: format!("v{n}"),
                            label: quality_label(width, height, (bandwidth > 0).then_some(bandwidth)),
                            bandwidth: (bandwidth > 0).then_some(bandwidth),
                            width,
                            height,
                            codecs,
                            group: Some("dash".into()),
                        },
                        source: Source::Ready(plan),
                    });
                }
                Kind::Audio => {
                    let n = audio.len();
                    let label = match (&lang, bandwidth) {
                        (Some(l), b) if b > 0 => format!("{l} · {} kbit/s", b / 1000),
                        (Some(l), _) => l.clone(),
                        (None, b) if b > 0 => format!("Audio · {} kbit/s", b / 1000),
                        _ => "Audio".into(),
                    };
                    audio.push(Entry {
                        option: TrackOption {
                            id: format!("a{n}"),
                            label,
                            language: lang.clone(),
                            group: Some("dash".into()),
                            default: n == 0,
                        },
                        source: Source::Ready(plan),
                    });
                }
                Kind::Text => {
                    let n = subtitles.len();
                    subtitles.push(Entry {
                        option: TrackOption {
                            id: format!("s{n}"),
                            label: lang.clone().unwrap_or_else(|| "Subtitles".into()),
                            language: lang.clone(),
                            group: None,
                            default: false,
                        },
                        source: Source::Ready(plan),
                    });
                }
            }
        }
    }
    if streams.is_empty() && audio.is_empty() {
        return Err(MediaError::invalid("the manifest lists no video or audio"));
    }
    // Best quality first (stable for equal bandwidth); ids follow the new order.
    streams.sort_by_key(|e| std::cmp::Reverse(e.option.bandwidth.unwrap_or(0)));
    for (i, e) in streams.iter_mut().enumerate() {
        e.option.id = format!("v{i}");
    }
    // Audio-only manifest: expose the audio tracks as the qualities instead.
    if streams.is_empty() {
        for (i, e) in audio.drain(..).enumerate() {
            streams.push(Entry {
                option: StreamOption {
                    id: format!("v{i}"),
                    label: e.option.label.clone(),
                    bandwidth: None,
                    width: None,
                    height: None,
                    codecs: None,
                    group: None,
                },
                source: e.source,
            });
        }
    }
    let analysis = Analysis {
        playlist: None,
        part_of_playlist: false,
        kind: StreamKind::Dash,
        title: title_from_url(base),
        duration_secs: duration,
        streams: streams.iter().map(|e| e.option.clone()).collect(),
        audio: audio.iter().map(|e| e.option.clone()).collect(),
        subtitles: subtitles.iter().map(|e| e.option.clone()).collect(),
    };
    Ok(Catalog { analysis, streams, audio, subtitles })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://cdn.example.com/dash/manifest.mpd").unwrap()
    }

    const NUMBERED: &str = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT30S">
 <BaseURL>media/</BaseURL>
 <Period>
  <AdaptationSet contentType="video" mimeType="video/mp4">
   <SegmentTemplate timescale="1000" duration="10000" startNumber="1" initialization="$RepresentationID$/init.mp4" media="$RepresentationID$/seg_$Number%03d$.m4s"/>
   <Representation id="low" bandwidth="500000" width="640" height="360" codecs="avc1.42c01e"/>
   <Representation id="high" bandwidth="3000000" width="1920" height="1080" codecs="avc1.640028"/>
  </AdaptationSet>
  <AdaptationSet contentType="audio" mimeType="audio/mp4" lang="en">
   <SegmentTemplate timescale="1000" duration="10000" initialization="a/init.mp4" media="a/$Number$.m4s"/>
   <Representation id="aud" bandwidth="128000"/>
  </AdaptationSet>
  <AdaptationSet contentType="text" mimeType="text/vtt" lang="fr">
   <Representation id="sub" bandwidth="1000"><BaseURL>subs/fr.vtt</BaseURL></Representation>
  </AdaptationSet>
 </Period>
</MPD>"#;

    #[test]
    fn a_numbered_template_manifest() {
        let c = parse(&base(), NUMBERED).unwrap();
        assert_eq!(c.analysis.duration_secs, Some(30.0));
        let labels: Vec<_> = c.analysis.streams.iter().map(|s| s.label.clone()).collect();
        assert_eq!(labels, ["1080p · 3.0 Mbit/s", "360p · 500 kbit/s"]);
        let Source::Ready(plan) = &c.streams[0].source else { panic!() };
        assert_eq!(plan.segments.len(), 3, "30 s / 10 s");
        assert_eq!(plan.init.as_ref().unwrap().url.as_str(), "https://cdn.example.com/dash/media/high/init.mp4");
        assert_eq!(plan.segments[0].url.as_str(), "https://cdn.example.com/dash/media/high/seg_001.m4s");
        assert_eq!(plan.segments[2].url.as_str(), "https://cdn.example.com/dash/media/high/seg_003.m4s");
        assert_eq!(plan.container, Container::Fmp4);
        assert_eq!(c.analysis.audio.len(), 1);
        assert_eq!(c.analysis.subtitles.len(), 1);
        let Source::Ready(sub) = &c.subtitles[0].source else { panic!() };
        assert_eq!((sub.container, sub.segments.len()), (Container::Vtt, 1));
        assert_eq!(sub.segments[0].url.as_str(), "https://cdn.example.com/dash/media/subs/fr.vtt");
    }

    #[test]
    fn a_timeline_manifest_with_repeats() {
        let mpd = r#"<MPD type="static" mediaPresentationDuration="PT20S"><Period><AdaptationSet mimeType="video/mp4">
 <SegmentTemplate timescale="90000" initialization="i.mp4" media="s_$Time$.m4s"><SegmentTimeline><S t="0" d="360000" r="1"/><S d="180000"/></SegmentTimeline></SegmentTemplate>
 <Representation id="1" bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        let c = parse(&base(), mpd).unwrap();
        let Source::Ready(p) = &c.streams[0].source else { panic!() };
        let urls: Vec<_> = p.segments.iter().map(|s| s.url.path().rsplit('/').next().unwrap().to_string()).collect();
        assert_eq!(urls, ["s_0.m4s", "s_360000.m4s", "s_720000.m4s"]);
    }

    #[test]
    fn a_negative_repeat_fills_the_period() {
        let mpd = r#"<MPD type="static" mediaPresentationDuration="PT10S"><Period><AdaptationSet mimeType="video/mp4">
 <SegmentTemplate timescale="1" media="s$Number$.m4s"><SegmentTimeline><S t="0" d="2" r="-1"/></SegmentTimeline></SegmentTemplate>
 <Representation id="1" bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        let c = parse(&base(), mpd).unwrap();
        let Source::Ready(p) = &c.streams[0].source else { panic!() };
        assert_eq!(p.segments.len(), 5);
    }

    #[test]
    fn a_segment_list_and_a_single_file() {
        let mpd = r#"<MPD type="static"><Period><AdaptationSet mimeType="video/mp4">
 <Representation id="l" bandwidth="1"><SegmentList><Initialization sourceURL="init.mp4"/><SegmentURL media="a.m4s"/><SegmentURL media="b.m4s"/></SegmentList></Representation>
 <Representation id="f" bandwidth="2"><BaseURL>whole.mp4</BaseURL><SegmentBase indexRange="0-100"/></Representation>
 </AdaptationSet></Period></MPD>"#;
        let c = parse(&base(), mpd).unwrap();
        assert_eq!(c.streams.len(), 2);
        let Source::Ready(whole) = &c.streams[0].source else { panic!() };
        assert_eq!((whole.segments.len(), whole.init.is_none()), (1, true));
        let Source::Ready(list) = &c.streams[1].source else { panic!() };
        assert_eq!((list.segments.len(), list.init.is_some()), (2, true));
    }

    #[test]
    fn refused_streams_say_why() {
        let live = r#"<MPD type="dynamic"><Period/></MPD>"#;
        assert!(matches!(parse(&base(), live), Err(MediaError::Unsupported(m)) if m.contains("live")));
        let drm = r#"<MPD type="static"><Period><AdaptationSet mimeType="video/mp4"><ContentProtection schemeIdUri="urn:uuid:edef8ba9"/><Representation id="1" bandwidth="1"><BaseURL>a.mp4</BaseURL></Representation></AdaptationSet></Period></MPD>"#;
        assert!(matches!(parse(&base(), drm), Err(MediaError::Unsupported(m)) if m.contains("DRM")));
        let two = r#"<MPD type="static"><Period id="1"/><Period id="2"/></MPD>"#;
        assert!(matches!(parse(&base(), two), Err(MediaError::Unsupported(m)) if m.contains("periods")));
        assert!(parse(&base(), "<html/>").is_err());
        assert!(parse(&base(), "").is_err());
        assert!(parse(&base(), "not xml at all <<<").is_err());
    }

    #[test]
    fn hostile_manifests_are_refused() {
        // A DTD (entity expansion / external entity tricks).
        let dtd = r#"<?xml version="1.0"?><!DOCTYPE MPD [<!ENTITY a "aaaa"><!ENTITY b "&a;&a;&a;&a;">]><MPD type="static"><Period/></MPD>"#;
        assert!(matches!(parse(&base(), dtd), Err(MediaError::Unsupported(m)) if m.contains("DTD")));
        // Nesting bomb.
        let deep = format!("{}{}", "<a>".repeat(limits::MAX_XML_DEPTH + 5), "</a>".repeat(limits::MAX_XML_DEPTH + 5));
        assert!(matches!(parse(&base(), &deep), Err(MediaError::Limit(_))));
        // Too many nodes.
        let many = format!("<MPD type=\"static\"><Period>{}</Period></MPD>", "<x/>".repeat(limits::MAX_XML_NODES + 1));
        assert!(matches!(parse(&base(), &many), Err(MediaError::Limit(_))));
        // Bad links.
        for link in ["file:///etc/passwd", "ftp://h/x.mp4", "http://insecure/x.mp4", "https://u:p@h/x.mp4"] {
            let mpd = format!(
                r#"<MPD type="static"><Period><AdaptationSet mimeType="video/mp4"><Representation id="1" bandwidth="1"><BaseURL>{link}</BaseURL></Representation></AdaptationSet></Period></MPD>"#
            );
            assert!(parse(&base(), &mpd).is_err(), "{link}");
        }
        // A template that would produce far too many segments.
        let mpd = r#"<MPD type="static" mediaPresentationDuration="P100Y"><Period><AdaptationSet mimeType="video/mp4"><SegmentTemplate timescale="1" duration="1" media="s$Number$.m4s"/><Representation id="1" bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        assert!(matches!(parse(&base(), mpd), Err(MediaError::Limit(_))));
        // Unknown template variable / unmatched dollar.
        assert!(expand("a$Bogus$b", "1", 1, Some(1), None).is_err());
        assert!(expand("a$Number", "1", 1, Some(1), None).is_err());
        assert!(expand("a$Number%99999d$", "1", 1, Some(1), None).is_err());
        assert!(expand("$Time$", "1", 1, Some(1), None).is_err(), "no time here");
    }

    #[test]
    fn template_expansion() {
        assert_eq!(expand("a/$RepresentationID$/$Number%05d$.m4s", "hd", 9, Some(42), None).unwrap(), "a/hd/00042.m4s");
        assert_eq!(expand("cost$$5", "x", 1, None, None).unwrap(), "cost$5");
        assert_eq!(expand("$Bandwidth$_$Time$", "x", 128, None, Some(7)).unwrap(), "128_7");
    }

    #[test]
    fn durations() {
        assert_eq!(iso_duration("PT1H2M3.5S"), Some(3723.5));
        assert_eq!(iso_duration("PT30S"), Some(30.0));
        assert_eq!(iso_duration("P1DT1H"), Some(90_000.0));
        assert_eq!(iso_duration("30"), None);
        assert_eq!(iso_duration("PT5"), None);
        assert_eq!(iso_duration("PTxS"), None);
    }
}
