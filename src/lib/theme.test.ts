import { beforeEach, describe, expect, it, vi } from "vitest";
import { applyTheme } from "./theme";

describe("applyTheme", () => {
  beforeEach(() => {
    // applyTheme only touches documentElement.dataset, so a bare stub is
    // enough — no need for a full DOM implementation.
    vi.stubGlobal("document", { documentElement: { dataset: {} as Record<string, string> } });
  });

  const attr = () => (document.documentElement.dataset as Record<string, string>).theme;

  it("pins an explicit choice so it wins over the OS preference", () => {
    applyTheme("dark");
    expect(attr()).toBe("dark");
    applyTheme("light");
    expect(attr()).toBe("light");
  });

  it("removes the attribute for 'system' so the media query decides", () => {
    applyTheme("dark");
    applyTheme("system");
    expect(attr()).toBeUndefined();
  });

  it("treats an unknown or missing value as 'system'", () => {
    // A settings file from an older build has no theme key at all.
    applyTheme("dark");
    applyTheme(undefined);
    expect(attr()).toBeUndefined();

    applyTheme("dark");
    applyTheme("solarized");
    expect(attr()).toBeUndefined();
  });
});
