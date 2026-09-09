import { describe, expect, it } from "vitest";
import { formatBytes, formatDate, formatEta } from "./api";

describe("formatBytes", () => {
  it("never shows raw bytes", () => {
    // The smallest unit is KB, so a handful of bytes rounds rather than
    // showing "37 B" next to "1.2 MB" in the same column.
    expect(formatBytes(0)).toBe("0 KB");
    expect(formatBytes(37)).toBe("0.0 KB");
    expect(formatBytes(1023)).toBe("1.0 KB");
  });

  it("uses one decimal below 10 and none above", () => {
    expect(formatBytes(9.5 * 1024 * 1024)).toBe("9.5 MB");
    expect(formatBytes(512 * 1024)).toBe("512 KB");
    expect(formatBytes(100 * 1024 * 1024)).toBe("100 MB");
  });

  it("steps up at exactly 1024 of a unit", () => {
    expect(formatBytes(1024 * 1024)).toBe("1.0 MB");
    expect(formatBytes(1024 * 1024 - 1)).toBe("1024 KB");
    expect(formatBytes(1024 ** 3)).toBe("1.0 GB");
    expect(formatBytes(1024 ** 4)).toBe("1.0 TB");
  });

  it("stops at TB rather than inventing a unit", () => {
    expect(formatBytes(5000 * 1024 ** 4)).toBe("5000 TB");
  });
});

describe("formatEta", () => {
  it("is blank when there is nothing meaningful to say", () => {
    // A stalled transfer divides by a zero speed; the row must show nothing
    // rather than "Infinity".
    expect(formatEta(Infinity)).toBe("");
    expect(formatEta(NaN)).toBe("");
    expect(formatEta(0)).toBe("");
    expect(formatEta(-5)).toBe("");
  });

  it("counts seconds under a minute", () => {
    expect(formatEta(1)).toBe("1s");
    expect(formatEta(59.4)).toBe("59s");
  });

  it("switches to minutes, then hours", () => {
    expect(formatEta(60)).toBe("1m 0s");
    expect(formatEta(3599)).toBe("59m 59s");
    expect(formatEta(3600)).toBe("1h 0m");
    expect(formatEta(3600 * 25 + 60 * 30)).toBe("25h 30m");
  });
});

describe("formatDate", () => {
  it("shows a dash for a missing timestamp", () => {
    // An entry from a queue file written before added_at existed.
    expect(formatDate(0)).toBe("—");
  });

  it("renders a real timestamp", () => {
    expect(formatDate(1_700_000_000)).not.toBe("—");
  });
});
