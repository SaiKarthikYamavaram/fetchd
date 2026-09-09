/// Multi-select bookkeeping, kept out of the component so the range rules can
/// be tested without rendering a list.
export type Selection = {
  ids: Set<string>;
  /// The last plainly-clicked row, which a shift-click extends from.
  anchor: string | null;
};

export const EMPTY: Selection = { ids: new Set(), anchor: null };

/// Toggle one row, or extend from the anchor when `extend` is set.
///
/// `order` is the list as currently filtered and displayed, so a range covers
/// what the user can actually see rather than the whole queue. A range only
/// ever adds: shift-clicking never clears, which is what every file manager
/// does and what makes repeated range picks predictable.
export function toggle(
  current: Selection,
  order: string[],
  id: string,
  extend: boolean,
): Selection {
  const ids = new Set(current.ids);
  const from = current.anchor ? order.indexOf(current.anchor) : -1;
  const to = order.indexOf(id);

  if (extend && from >= 0 && to >= 0) {
    const [lo, hi] = from < to ? [from, to] : [to, from];
    for (let i = lo; i <= hi; i++) ids.add(order[i]);
    // The anchor stays put, so a second shift-click re-ranges from the same
    // origin rather than walking along behind the cursor.
    return { ids, anchor: current.anchor };
  }

  if (ids.has(id)) ids.delete(id);
  else ids.add(id);
  return { ids, anchor: id };
}

/// Select every visible row, or clear if they are all selected already.
export function toggleAll(current: Selection, order: string[]): Selection {
  const allOn = order.length > 0 && order.every((id) => current.ids.has(id));
  return { ids: allOn ? new Set() : new Set(order), anchor: null };
}

/// Drop ids that have left the queue. Returns the same object when nothing
/// changed, so React can skip the re-render.
export function prune(current: Selection, live: string[]): Selection {
  if (current.ids.size === 0) return current;
  const alive = new Set(live);
  const ids = new Set([...current.ids].filter((id) => alive.has(id)));
  if (ids.size === current.ids.size) return current;
  return { ids, anchor: current.anchor && alive.has(current.anchor) ? current.anchor : null };
}
