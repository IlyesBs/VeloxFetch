import { describe, expect, it } from "vitest";
import { linksIn } from "./App";
import { canOpen } from "./components/FileIcon";
import { dayGroup } from "./views/HistoryView";

describe("links in pasted or dropped text", () => {
  it("keeps http(s) links only, in order, once each", () => {
    expect(linksIn("see https://a.test/x.zip and http://b.test/y.iso\nhttps://a.test/x.zip ftp://c.test/z")).toEqual(["https://a.test/x.zip", "http://b.test/y.iso"]);
  });
  it("strips the brackets and punctuation around a link", () => {
    expect(linksIn("<https://a.test/f.zip>, (https://b.test/g.zip).")).toEqual(["https://a.test/f.zip", "https://b.test/g.zip"]);
  });
  it("ignores text without links and broken ones", () => {
    expect(linksIn("hello world")).toEqual([]);
    expect(linksIn("https:// http://[bad")).toEqual([]);
    expect(linksIn("javascript:alert(1) file:///etc/passwd")).toEqual([]);
  });
  it("stops at 1000 links", () => {
    const many = Array.from({ length: 1500 }, (_, i) => `https://h.test/${i}`).join(" ");
    expect(linksIn(many)).toHaveLength(1000);
  });
});

describe("what may be opened", () => {
  it("never offers to open programs, scripts, shortcuts or pages", () => {
    for (const n of ["setup.exe", "a.MSI", "run.bat", "x.ps1", "s.js", "l.lnk", "p.html", "i.svg", "a.dmg", "a.AppImage", "setup.exe.", "noext"]) {
      expect(canOpen(n, "other")).toBe(false);
    }
    expect(canOpen("tool.zip", "program")).toBe(false);
  });
  it("offers it for media, documents and archives", () => {
    for (const n of ["movie.mp4", "song.mp3", "photo.jpg", "report.pdf", "archive.zip"]) expect(canOpen(n, "document")).toBe(true);
  });
});

describe("history day groups", () => {
  const now = new Date(2026, 8, 24, 15, 0, 0);
  const at = (d: Date) => Math.floor(d.getTime() / 1000);
  it("groups by calendar day, not by 24-hour windows", () => {
    expect(dayGroup(at(new Date(2026, 8, 24, 0, 1)), now)).toBe("Today");
    expect(dayGroup(at(new Date(2026, 8, 23, 23, 59)), now)).toBe("Yesterday");
    expect(dayGroup(at(new Date(2026, 8, 20, 12, 0)), now)).toBe("Earlier this week");
    expect(dayGroup(at(new Date(2026, 7, 1, 12, 0)), now)).toBe("Earlier");
  });
});
