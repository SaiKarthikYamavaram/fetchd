import { describe, expect, it } from "vitest";
import { isVideoUrl, suggestedName } from "./AddDialog";

describe("isVideoUrl", () => {
  // Decides only whether the quality picker is offered, so drift with the
  // backend's VIDEO_HOSTS hides an option rather than breaking a download.
  it("matches known hosts and their subdomains", () => {
    expect(isVideoUrl("https://www.youtube.com/watch?v=abc")).toBe(true);
    expect(isVideoUrl("https://m.youtube.com/watch?v=abc")).toBe(true);
    expect(isVideoUrl("https://youtu.be/abc")).toBe(true);
    expect(isVideoUrl("https://clips.twitch.tv/foo")).toBe(true);
  });

  it("is case-insensitive on the host", () => {
    expect(isVideoUrl("https://WWW.YouTube.COM/watch?v=abc")).toBe(true);
  });

  it("rejects lookalike hosts", () => {
    // The suffix must fall on a label boundary.
    expect(isVideoUrl("https://evilyoutube.com/watch?v=abc")).toBe(false);
    expect(isVideoUrl("https://youtube.com.evil.test/watch?v=abc")).toBe(false);
  });

  it("rejects ordinary files and unparseable input", () => {
    expect(isVideoUrl("https://example.com/file.zip")).toBe(false);
    expect(isVideoUrl("not a url")).toBe(false);
    expect(isVideoUrl("")).toBe(false);
  });
});

describe("suggestedName", () => {
  it("takes the last path segment", () => {
    expect(suggestedName("https://example.com/files/report.pdf")).toBe("report.pdf");
  });

  it("ignores the query and fragment", () => {
    expect(suggestedName("https://example.com/a/b.zip?token=xyz#top")).toBe("b.zip");
  });

  it("decodes percent-escapes so the hint is readable", () => {
    expect(suggestedName("https://example.com/my%20file.zip")).toBe("my file.zip");
  });

  it("ignores a trailing slash rather than suggesting an empty name", () => {
    expect(suggestedName("https://example.com/downloads/")).toBe("downloads");
  });

  it("is blank when there is nothing to suggest", () => {
    // The field then shows "Automatic", which is what a blank name means.
    expect(suggestedName("https://example.com/")).toBe("");
    expect(suggestedName("not a url")).toBe("");
  });

  it("does not throw on a malformed escape", () => {
    // decodeURIComponent throws on "%zz"; the caller must still get a string.
    expect(() => suggestedName("https://example.com/bad%zz.zip")).not.toThrow();
  });
});
