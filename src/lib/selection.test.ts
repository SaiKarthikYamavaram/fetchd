import { describe, expect, it } from "vitest";
import { EMPTY, prune, toggle, toggleAll, type Selection } from "./selection";

const LIST = ["a", "b", "c", "d", "e"];

function sel(ids: string[], anchor: string | null = null): Selection {
  return { ids: new Set(ids), anchor };
}
const picked = (s: Selection) => [...s.ids].sort();

describe("toggle", () => {
  it("adds then removes on a plain click", () => {
    const one = toggle(EMPTY, LIST, "c", false);
    expect(picked(one)).toEqual(["c"]);
    expect(one.anchor).toBe("c");

    const none = toggle(one, LIST, "c", false);
    expect(picked(none)).toEqual([]);
    // The anchor stays on the row last clicked, selected or not, so a shift
    // click straight after a deselect still has an origin.
    expect(none.anchor).toBe("c");
  });

  it("never mutates the selection it was given", () => {
    const before = sel(["a"]);
    toggle(before, LIST, "b", false);
    expect(picked(before)).toEqual(["a"]);
  });

  it("extends downwards from the anchor", () => {
    const start = toggle(EMPTY, LIST, "b", false);
    const range = toggle(start, LIST, "d", true);
    expect(picked(range)).toEqual(["b", "c", "d"]);
  });

  it("extends upwards from the anchor", () => {
    const start = toggle(EMPTY, LIST, "d", false);
    const range = toggle(start, LIST, "b", true);
    expect(picked(range)).toEqual(["b", "c", "d"]);
  });

  it("keeps the anchor so a second shift-click re-ranges from the same origin", () => {
    const start = toggle(EMPTY, LIST, "b", false);
    const far = toggle(start, LIST, "e", true);
    expect(picked(far)).toEqual(["b", "c", "d", "e"]);
    expect(far.anchor).toBe("b");

    // Shrinking the range does not deselect what the wider one picked up —
    // a range only ever adds.
    const near = toggle(far, LIST, "c", true);
    expect(picked(near)).toEqual(["b", "c", "d", "e"]);
  });

  it("adds to what is already selected rather than replacing it", () => {
    const start = sel(["a"], "c");
    const range = toggle(start, LIST, "d", true);
    expect(picked(range)).toEqual(["a", "c", "d"]);
  });

  it("shift with no anchor behaves like a plain click", () => {
    const first = toggle(EMPTY, LIST, "c", true);
    expect(picked(first)).toEqual(["c"]);
    expect(first.anchor).toBe("c");
  });

  it("shift on a range of one selects just that row", () => {
    const start = toggle(EMPTY, LIST, "c", false);
    const same = toggle(start, LIST, "c", true);
    expect(picked(same)).toEqual(["c"]);
  });

  it("ignores an anchor that is no longer in the visible list", () => {
    // Switching filters can leave the anchor off-screen; the click must still
    // register instead of doing nothing.
    const stale = sel([], "zzz");
    const next = toggle(stale, LIST, "b", true);
    expect(picked(next)).toEqual(["b"]);
    expect(next.anchor).toBe("b");
  });

  it("ranges over the filtered order, not the whole queue", () => {
    // "Active" filter showing only a, c, e: a shift from a to e must not pick
    // up the hidden b and d.
    const shown = ["a", "c", "e"];
    const start = toggle(EMPTY, shown, "a", false);
    const range = toggle(start, shown, "e", true);
    expect(picked(range)).toEqual(["a", "c", "e"]);
  });
});

describe("toggleAll", () => {
  it("selects everything visible, then clears", () => {
    const all = toggleAll(EMPTY, LIST);
    expect(picked(all)).toEqual(LIST);
    expect(picked(toggleAll(all, LIST))).toEqual([]);
  });

  it("selects the rest when only some are picked", () => {
    expect(picked(toggleAll(sel(["a", "b"]), LIST))).toEqual(LIST);
  });

  it("covers only the filtered rows", () => {
    const shown = ["a", "c"];
    expect(picked(toggleAll(EMPTY, shown))).toEqual(shown);
  });

  it("does nothing on an empty list", () => {
    expect(picked(toggleAll(EMPTY, []))).toEqual([]);
  });
});

describe("prune", () => {
  it("drops ids that have left the queue", () => {
    const next = prune(sel(["a", "gone"], "a"), LIST);
    expect(picked(next)).toEqual(["a"]);
  });

  it("returns the same object when nothing changed, so React can skip", () => {
    const before = sel(["a", "b"], "a");
    expect(prune(before, LIST)).toBe(before);
  });

  it("returns the same object for an empty selection", () => {
    expect(prune(EMPTY, [])).toBe(EMPTY);
  });

  it("clears an anchor that is gone but keeps one that survives", () => {
    expect(prune(sel(["a", "gone"], "gone"), LIST).anchor).toBeNull();
    expect(prune(sel(["a", "gone"], "a"), LIST).anchor).toBe("a");
  });

  it("empties the selection when the whole queue is cleared", () => {
    expect(picked(prune(sel(["a", "b"], "a"), []))).toEqual([]);
  });
});
