import { describe, expect, it } from "vitest";
import { categoryOf, extensionOf, kindOf } from "./filetype";

describe("extensionOf", () => {
  it("takes the final dotted segment, lowercased", () => {
    expect(extensionOf("movie.MP4")).toBe("mp4");
    expect(extensionOf("archive.tar.gz")).toBe("gz");
  });

  it("returns nothing when there is no extension to speak of", () => {
    expect(extensionOf("README")).toBe("");
    // A dotfile's leading dot is not an extension marker.
    expect(extensionOf(".bashrc")).toBe("");
    // A trailing dot leaves nothing after it.
    expect(extensionOf("weird.")).toBe("");
    expect(extensionOf("")).toBe("");
  });

  it("ignores directories in the name", () => {
    expect(extensionOf("/home/u/Downloads/clip.mkv")).toBe("mkv");
    // A dot in a folder must not be read as the file's extension.
    expect(extensionOf("/home/u/v1.2/README")).toBe("");
  });
});

describe("kindOf", () => {
  it("buckets the common cases", () => {
    expect(kindOf("clip.mkv")).toBe("video");
    expect(kindOf("song.flac")).toBe("audio");
    expect(kindOf("backup.tar.gz")).toBe("archive");
    expect(kindOf("photo.HEIC")).toBe("image");
    expect(kindOf("report.pdf")).toBe("doc");
  });

  it("separates the office types rather than lumping them as documents", () => {
    expect(kindOf("budget.xlsx")).toBe("sheet");
    expect(kindOf("deck.pptx")).toBe("slides");
    expect(kindOf("novel.epub")).toBe("book");
  });

  it("covers what a download manager actually receives", () => {
    expect(kindOf("ubuntu.iso")).toBe("disc");
    expect(kindOf("app.AppImage")).toBe("package");
    expect(kindOf("tool.deb")).toBe("package");
    expect(kindOf("app.apk")).toBe("package");
    expect(kindOf("season01.torrent")).toBe("torrent");
    expect(kindOf("movie.srt")).toBe("subs");
    expect(kindOf("Inter.woff2")).toBe("font");
    expect(kindOf("script.py")).toBe("code");
  });

  it("treats a stream manifest as video", () => {
    // It is routed to yt-dlp and produces a video, so the tile should say so
    // rather than showing a generic file.
    expect(kindOf("master.m3u8")).toBe("video");
    expect(kindOf("manifest.mpd")).toBe("video");
  });

  it("reads .ts as a transport stream, not TypeScript", () => {
    // Both are real, but only one of them gets downloaded.
    expect(kindOf("segment.ts")).toBe("video");
  });

  it("falls back to a plain file for anything unknown", () => {
    expect(kindOf("download.bin")).toBe("disc"); // .bin is a disc image
    expect(kindOf("mystery.qqq")).toBe("file");
    expect(kindOf("README")).toBe("file");
  });
});

describe("categoryOf", () => {
  it("groups video and audio as media", () => {
    expect(categoryOf("clip.mkv")).toBe("media");
    expect(categoryOf("song.flac")).toBe("media");
  });

  it("groups office kinds as documents", () => {
    expect(categoryOf("report.pdf")).toBe("documents");
    expect(categoryOf("budget.xlsx")).toBe("documents");
    expect(categoryOf("deck.pptx")).toBe("documents");
    expect(categoryOf("novel.epub")).toBe("documents");
  });

  it("groups compressed and installable kinds as archives", () => {
    expect(categoryOf("backup.tar.gz")).toBe("archives");
    expect(categoryOf("tool.deb")).toBe("archives");
    expect(categoryOf("ubuntu.iso")).toBe("archives");
    expect(categoryOf("season01.torrent")).toBe("archives");
  });

  it("falls back to other for the rest", () => {
    expect(categoryOf("photo.png")).toBe("other");
    expect(categoryOf("script.py")).toBe("other");
    expect(categoryOf("README")).toBe("other");
  });
});
