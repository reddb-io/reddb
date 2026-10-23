// SplitView's behavior, with no DOM in sight.
//
// Everything a resizable split does that is worth getting right is arithmetic:
// where a pointer lands inside a box, how far an arrow key moves the divider,
// and what stops either from collapsing a pane to nothing. Kept here, that
// arithmetic is testable without a browser, and SplitView.svelte is left
// holding only the parts that genuinely need one — the element, the pointer
// capture and the keyboard listener.
//
// The unit throughout is a *fraction* of the container, never a pixel width: a
// split expressed in pixels stops meaning anything the moment its container
// resizes, and a Kit component has no idea how big its container will be in an
// application it has never seen.

/** The axis the panes are laid out along. */
export const SPLIT_ORIENTATIONS = ["horizontal", "vertical"] as const;
export type SplitOrientation = (typeof SPLIT_ORIENTATIONS)[number];

/** How much of the container the start pane may take. */
export interface SplitBounds {
  /** Smallest fraction the start pane may shrink to. */
  min: number;
  /** Largest fraction it may grow to. */
  max: number;
}

/**
 * The default bounds. Neither pane can go below a tenth: a pane dragged to
 * zero looks like a bug in the application rather than a position of the
 * divider, and nothing in the DOM would tell a user it is still there.
 */
export const SPLIT_BOUNDS: SplitBounds = { min: 0.1, max: 0.9 };

/** How far one arrow key press moves the divider. */
export const SPLIT_STEP = 0.02;

/** `bounds`, in the order the arithmetic needs them whichever way round they came. */
function ordered(bounds: SplitBounds): SplitBounds {
  return { min: Math.min(bounds.min, bounds.max), max: Math.max(bounds.min, bounds.max) };
}

/**
 * `fraction`, held inside `bounds`.
 *
 * A fraction that is not a finite number is a caller's bug, and it resolves to
 * the midpoint rather than propagating: `NaN` would reach the DOM as a style
 * the browser drops, leaving a split that is silently unsplit.
 */
export function clampFraction(fraction: number, bounds: SplitBounds = SPLIT_BOUNDS): number {
  const { min, max } = ordered(bounds);
  if (!Number.isFinite(fraction)) return clampFraction(0.5, bounds);
  return Math.min(max, Math.max(min, fraction));
}

/**
 * The fraction a pointer `offset` from the container's leading edge means,
 * given the container's `size` along the same axis.
 *
 * `null` when the container has no measurable size — which is what a hidden
 * container, or one measured before layout, gives back. A drag that cannot be
 * measured must leave the divider where it is, not slam it to an edge.
 */
export function fractionAt(
  offset: number,
  size: number,
  bounds: SplitBounds = SPLIT_BOUNDS,
): number | null {
  if (!Number.isFinite(offset) || !Number.isFinite(size) || size <= 0) return null;
  return clampFraction(offset / size, bounds);
}

/**
 * The fraction `key` moves the divider to, or `null` when the key means
 * nothing here.
 *
 * `null` is the important half: the keys of the other axis, and every key that
 * is not a movement, have to reach the page. A separator that swallowed every
 * arrow would take the page's scrolling with it.
 */
export function fractionForKey(
  key: string,
  fraction: number,
  orientation: SplitOrientation,
  bounds: SplitBounds = SPLIT_BOUNDS,
  step: number = SPLIT_STEP,
): number | null {
  const current = clampFraction(fraction, bounds);
  const { min, max } = ordered(bounds);

  if (key === "Home") return min;
  if (key === "End") return max;

  const back = orientation === "horizontal" ? "ArrowLeft" : "ArrowUp";
  const forward = orientation === "horizontal" ? "ArrowRight" : "ArrowDown";
  if (key === back) return clampFraction(current - step, bounds);
  if (key === forward) return clampFraction(current + step, bounds);

  return null;
}

/** `0.425` -> `"42.5%"`, at a tenth of a percent — enough for a pixel either way. */
export function percentOf(fraction: number): string {
  return `${Math.round(clampFraction(fraction, { min: 0, max: 1 }) * 1000) / 10}%`;
}

/**
 * The separator's own orientation, which is the opposite of the split's: panes
 * side by side are divided by a vertical rule. ARIA names the separator, not
 * the layout, so this is a translation and not a synonym.
 */
export function separatorOrientation(orientation: SplitOrientation): SplitOrientation {
  return orientation === "horizontal" ? "vertical" : "horizontal";
}
