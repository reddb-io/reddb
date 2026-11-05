// SplitView's behavior module, tested without a browser.
//
// This is the half of the component that is worth being paranoid about: a
// divider is arithmetic, and every way a split goes wrong — a pane collapsed
// to nothing, a NaN width the browser silently drops, a page that stops
// scrolling because the separator ate the arrow keys — is a case in one of
// these four functions. None of them needs a DOM to be wrong, so none of them
// needs one to be checked.

import { describe, expect, it } from "vitest";
import {
  SPLIT_BOUNDS,
  SPLIT_ORIENTATIONS,
  SPLIT_STEP,
  clampFraction,
  fractionAt,
  fractionForKey,
  percentOf,
  separatorOrientation,
} from "../src/primitives/split-view.behavior";

describe("clampFraction", () => {
  it("leaves a fraction inside the bounds alone", () => {
    expect(clampFraction(0.5)).toBe(0.5);
    expect(clampFraction(0.25)).toBe(0.25);
  });

  it("stops either pane collapsing to nothing", () => {
    expect(clampFraction(0)).toBe(SPLIT_BOUNDS.min);
    expect(clampFraction(1)).toBe(SPLIT_BOUNDS.max);
    expect(clampFraction(-3)).toBe(SPLIT_BOUNDS.min);
    expect(clampFraction(12)).toBe(SPLIT_BOUNDS.max);
  });

  it("takes the caller's own bounds", () => {
    expect(clampFraction(0.05, { min: 0.02, max: 0.5 })).toBe(0.05);
    expect(clampFraction(0.9, { min: 0.02, max: 0.5 })).toBe(0.5);
  });

  it("resolves a fraction that is not a number to the midpoint", () => {
    // NaN would reach the DOM as a style the browser drops, leaving a split
    // that is silently unsplit — worse than a divider in the wrong place.
    expect(clampFraction(Number.NaN)).toBe(0.5);
    expect(clampFraction(Number.POSITIVE_INFINITY)).toBe(0.5);
  });

  it("reads bounds given the wrong way round rather than inverting the split", () => {
    expect(clampFraction(0.9, { min: 0.8, max: 0.2 })).toBe(0.8);
  });
});

describe("fractionAt", () => {
  it("reads a pointer offset as a share of the container", () => {
    expect(fractionAt(50, 200)).toBe(0.25);
    expect(fractionAt(160, 200)).toBe(0.8);
  });

  it("holds the result inside the bounds", () => {
    expect(fractionAt(0, 200)).toBe(SPLIT_BOUNDS.min);
    expect(fractionAt(400, 200)).toBe(SPLIT_BOUNDS.max);
  });

  it("refuses a container it cannot measure", () => {
    // A hidden container, or one measured before layout, reports zero — and a
    // drag that cannot be measured must leave the divider where it is.
    expect(fractionAt(50, 0)).toBeNull();
    expect(fractionAt(50, -10)).toBeNull();
    expect(fractionAt(Number.NaN, 200)).toBeNull();
    expect(fractionAt(50, Number.NaN)).toBeNull();
  });
});

describe("fractionForKey", () => {
  it("moves the divider by one step along its own axis", () => {
    expect(fractionForKey("ArrowRight", 0.5, "horizontal")).toBeCloseTo(0.5 + SPLIT_STEP);
    expect(fractionForKey("ArrowLeft", 0.5, "horizontal")).toBeCloseTo(0.5 - SPLIT_STEP);
    expect(fractionForKey("ArrowDown", 0.5, "vertical")).toBeCloseTo(0.5 + SPLIT_STEP);
    expect(fractionForKey("ArrowUp", 0.5, "vertical")).toBeCloseTo(0.5 - SPLIT_STEP);
  });

  it("leaves the other axis' keys to the page", () => {
    // A separator that swallowed every arrow would take the page's scrolling
    // with it.
    for (const key of ["ArrowUp", "ArrowDown"]) {
      expect(fractionForKey(key, 0.5, "horizontal")).toBeNull();
    }
    for (const key of ["ArrowLeft", "ArrowRight"]) {
      expect(fractionForKey(key, 0.5, "vertical")).toBeNull();
    }
  });

  it("leaves every key that is not a movement to the page", () => {
    for (const key of ["Tab", "Enter", " ", "a", "Escape", "PageDown"]) {
      expect(fractionForKey(key, 0.5, "horizontal")).toBeNull();
    }
  });

  it("takes the divider to either end", () => {
    for (const orientation of SPLIT_ORIENTATIONS) {
      expect(fractionForKey("Home", 0.5, orientation)).toBe(SPLIT_BOUNDS.min);
      expect(fractionForKey("End", 0.5, orientation)).toBe(SPLIT_BOUNDS.max);
    }
  });

  it("stops at the bounds rather than stepping past them", () => {
    expect(fractionForKey("ArrowLeft", SPLIT_BOUNDS.min, "horizontal")).toBe(SPLIT_BOUNDS.min);
    expect(fractionForKey("ArrowRight", SPLIT_BOUNDS.max, "horizontal")).toBe(SPLIT_BOUNDS.max);
  });

  it("steps from a fraction that was already out of bounds", () => {
    expect(fractionForKey("ArrowRight", -5, "horizontal")).toBeCloseTo(
      SPLIT_BOUNDS.min + SPLIT_STEP,
    );
  });
});

describe("percentOf", () => {
  it("writes a fraction as a CSS percentage", () => {
    expect(percentOf(0.5)).toBe("50%");
    expect(percentOf(0.425)).toBe("42.5%");
  });

  it("never writes a percentage the browser would drop", () => {
    expect(percentOf(Number.NaN)).toBe("50%");
    expect(percentOf(-1)).toBe("0%");
    expect(percentOf(2)).toBe("100%");
  });
});

describe("separatorOrientation", () => {
  it("names the rule, not the layout", () => {
    // Panes side by side are divided by a vertical rule. ARIA describes the
    // separator, so this is a translation and not a synonym.
    expect(separatorOrientation("horizontal")).toBe("vertical");
    expect(separatorOrientation("vertical")).toBe("horizontal");
  });
});
