// The Logo's physics: clearspace and minimum size, as arithmetic.
//
// ADR 0004 settled that these are the component's physics and not its
// documentation — "the Logo's box includes the clearspace intrinsically and
// below-minimum is a dev error. No opt-out prop; a legitimate exception must
// be written by the Brand first." That has one mechanical consequence, and it
// is the whole of this module: the box a caller lays out is mark + clearspace,
// so a caller cannot place something inside the clearspace by accident, and
// cannot ask for the mark without it at all.
//
// Everything is measured against the SYMBOL — the red glyph — because the
// symbol is the same drawing in every layout. Clearspace is 25% of its height;
// the minimum is a floor under its height. One rule, three layouts, and no
// per-layout numbers to keep in step: the stacked Mark's minimum is taller
// than the horizontal one's only because its symbol is a smaller share of it.
//
// These are lengths in CSS pixels rather than tokens, and deliberately so. A
// Theme reassigns colour; a density stop reassigns the space a component holds
// inside itself (ADR 0003). Clearspace is neither — it is a proportion of the
// Brand's drawing, fixed by the licence, and a stop that shrank it would be
// shrinking the Brand's own margin.

import type { Mark } from "./logo.marks";

/** Clearspace is 25% of the symbol's height (ADR 0004). */
export const CLEARSPACE_RATIO = 0.25;

/**
 * The floor under the symbol, in CSS pixels. Below it the wordmark's counters
 * close up and the notch in the symbol stops reading, so it is an error rather
 * than a smaller Logo.
 */
export const MINIMUM_SYMBOL_PX = 16;

/** What `size` means when a caller does not say: comfortable, not minimal. */
export const DEFAULT_SYMBOL_PX = 32;

/** The Logo's laid-out geometry, in CSS pixels. */
export interface LogoBox {
  /** The symbol's height — the size the caller asked for. */
  symbol: number;
  /** Clearspace on every side. */
  clearspace: number;
  /** The Mark itself, inside the clearspace. */
  markWidth: number;
  markHeight: number;
  /** The component's own box: the Mark plus clearspace on all four sides. */
  width: number;
  height: number;
}

/**
 * The refusal for a Logo asked to render below the minimum.
 *
 * ADR 0004 calls it a dev error, and an error is what it is: a Logo that
 * renders illegibly is worse than a page with a hole in it, because nobody
 * files the second one as a bug against the DS.
 */
export class LogoTooSmallError extends Error {
  /** The symbol height that was asked for, in CSS pixels. */
  readonly size: number;
  /** The floor it fell under. */
  readonly minimum: number;

  constructor(size: number, minimum: number = MINIMUM_SYMBOL_PX) {
    super(
      `Logo: size=${size} would render the symbol at ${size}px, below the ${minimum}px minimum.\n` +
        "Minimum size is the component's physics, not its documentation (ADR 0004): there is no " +
        "opt-out prop, because a legitimate exception has to be written by the Brand first.\n" +
        `Ask for size=${minimum} or larger, or use a layout whose symbol carries at this size.`,
    );
    this.name = "LogoTooSmallError";
    this.size = size;
    this.minimum = minimum;
  }
}

/**
 * The box `mark` occupies when its symbol is `size` pixels tall.
 *
 * Throws rather than clamping when `size` is below the minimum or is not a
 * usable length: clamping would render a Logo nobody asked for, and silence is
 * how a below-minimum Logo ships.
 */
export function logoBox(mark: Mark, size: number = DEFAULT_SYMBOL_PX): LogoBox {
  if (!Number.isFinite(size) || size <= 0) {
    throw new LogoTooSmallError(size);
  }
  if (size < MINIMUM_SYMBOL_PX) {
    throw new LogoTooSmallError(size);
  }

  // The symbol is a share of the drawing's height, so the drawing scales by
  // the ratio between them — which is what keeps one `size` meaning the same
  // thing across layouts whose proportions differ.
  const scale = size / mark.symbolHeight;
  const markWidth = mark.width * scale;
  const markHeight = mark.height * scale;
  const clearspace = size * CLEARSPACE_RATIO;

  return {
    symbol: size,
    clearspace,
    markWidth,
    markHeight,
    width: markWidth + clearspace * 2,
    height: markHeight + clearspace * 2,
  };
}

/**
 * The smallest `size` a Mark may be placed at. One number for every Mark,
 * because the floor is under the symbol and the symbol is shared.
 */
export function minimumSize(): number {
  return MINIMUM_SYMBOL_PX;
}

/**
 * A length as CSS, rounded to a resolution finer than a device pixel.
 *
 * The Marks' viewBoxes carry three decimals, so the arithmetic above produces
 * numbers like 120.62144 — real, and not worth the bytes. Rounding here rather
 * than in `logoBox` keeps the box exact for anyone computing with it.
 */
export function px(length: number): string {
  return `${Math.round(length * 1000) / 1000}px`;
}
