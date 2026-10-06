import { describe, expect, it } from "vitest";
import { formatBytes, formatEta, formatRelative, formatSpeed, fraction, hostOf, isStreamLink, isVideoPage, opensVideoDialog, parseRate, percent } from "./format";

describe("format", () => {
  it("formats bytes and speeds", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(1023)).toBe("1023 B");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(1024 ** 2 * 18.4)).toBe("18.4 MB");
    expect(formatBytes(1024 ** 3 * 3)).toBe("3.0 GB");
    expect(formatBytes(150 * 1024 ** 2)).toBe("150 MB");
    expect(formatBytes(null)).toBe("—");
    expect(formatBytes(Number.NaN)).toBe("—");
    expect(formatSpeed(0)).toBe("0 B/s");
    expect(formatSpeed(2 * 1024 ** 2)).toBe("2.0 MB/s");
  });

  it("formats ETA", () => {
    expect(formatEta(null)).toBe("—");
    expect(formatEta(0)).toBe("0:00");
    expect(formatEta(92)).toBe("1:32");
    expect(formatEta(3725)).toBe("1:02:05");
  });

  it("computes progress safely", () => {
    expect(percent(50, 200)).toBe(25);
    expect(percent(5, 0)).toBe(0);
    expect(percent(5, null)).toBe(0);
    expect(percent(500, 100)).toBe(100);
    expect(fraction(1, 4)).toBe(0.25);
  });

  it("parses speed limits", () => {
    expect(parseRate("500K")).toBe(500 * 1024);
    expect(parseRate("2M")).toBe(2 * 1024 ** 2);
    expect(parseRate("1.5 mb/s")).toBe(1.5 * 1024 ** 2);
    expect(parseRate("1000")).toBe(1000);
    for (const bad of ["", "abc", "0", "-5", "5X", "1e9"]) expect(parseRate(bad)).toBeNull();
  });

  it("formats relative time and hosts", () => {
    const now = 1_000_000_000_000;
    expect(formatRelative(now / 1000 - 5, now)).toBe("just now");
    expect(formatRelative(now / 1000 - 600, now)).toBe("10 min ago");
    expect(formatRelative(now / 1000 - 7200, now)).toBe("2 h ago");
    expect(formatRelative(now / 1000 - 86400 * 3, now)).toBe("3 d ago");
    expect(hostOf("https://cdn.example.com/a/b.zip")).toBe("cdn.example.com");
    expect(hostOf("not a url")).toBe("not a url");
  });

  it("recognises video stream links", () => {
    expect(isStreamLink("https://cdn.test/a/master.m3u8")).toBe(true);
    expect(isStreamLink("https://cdn.test/a/Manifest.MPD?token=1#x")).toBe(true);
    expect(isStreamLink(" https://cdn.test/a/master.m3u8 ")).toBe(true);
    expect(isStreamLink("https://cdn.test/a/file.zip")).toBe(false);
    expect(isStreamLink("https://cdn.test/m3u8/")).toBe(false);
    expect(isStreamLink("https://cdn.test/a.m3u8.zip")).toBe(false);
    expect(isStreamLink("not a url")).toBe(false);
    expect(isStreamLink("")).toBe(false);
  });

  it("recognises video pages of known sites and nothing else", () => {
    for (const yes of ["https://www.youtube.com/watch?v=abc", "https://youtu.be/abc", "https://m.youtube.com/shorts/x", "https://vimeo.com/1", "https://x.com/a/status/1"]) {
      expect(isVideoPage(yes)).toBe(true);
      expect(opensVideoDialog(yes)).toBe(true);
    }
    for (const no of ["https://www.youtube.com/", "https://youtube.com.evil.test/watch?v=1", "https://notyoutube.com/watch", "https://example.com/v.mp4", "javascript:alert(1)", ""]) {
      expect(isVideoPage(no)).toBe(false);
    }
    expect(opensVideoDialog("https://cdn.test/master.m3u8")).toBe(true);
    expect(opensVideoDialog("https://cdn.test/file.zip")).toBe(false);
  });
});
