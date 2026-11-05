// The Marks the Logo may place, and the refusal when it may not.
//
// The Brand owns the drawing; the DS owns only the placement (ADR 0004). So
// this module enumerates what the pinned Assets release ships and nothing
// else: five layouts, two surfaces, and one hole where the release ships no
// Mark. It never composes, recolors or substitutes a Mark — the asset licence
// binds that literally, and a variant that does not exist upstream is a Brand
// proposal, not a local file.
//
// A Mark is reached by importing the file, so it stays opaque bytes the whole
// way through: the bundler emits the SVG as an asset and hands back its URL,
// which is what the Logo renders as an <img>. Nothing here parses the drawing,
// and no code path can alter it — "reproduced unaltered" is therefore a
// property of how the Logo works rather than a rule someone has to follow.
//
// `on` names the SURFACE behind the Mark, never the Theme: a dark hero on a
// light marketing page takes `on="dark"` under the light Color Scheme, which is
// exactly the case ADR 0004 fixed the prop for.

import horizontalColor from "./marks/reddb-horizontal-color.svg";
import horizontalInverse from "./marks/reddb-horizontal-inverse.svg";
import iconColor from "./marks/reddb-icon-color.svg";
import stackedColor from "./marks/reddb-stacked-color.svg";
import stackedInverse from "./marks/reddb-stacked-inverse.svg";
import symbolBlack from "./marks/reddb-symbol-black.svg";
import symbolColor from "./marks/reddb-symbol-color.svg";
import symbolKnockout from "./marks/reddb-symbol-knockout.svg";
import symbolWhite from "./marks/reddb-symbol-white.svg";
import wordmarkBlack from "./marks/reddb-wordmark-black.svg";
import wordmarkWhite from "./marks/reddb-wordmark-white.svg";

/**
 * The pinned Brand Assets release these Marks came from
 * (`vendor/brand/brand.lock.json`).
 *
 * Written here as a literal because a Kit is vendorable source with no build
 * step and no lock to read at runtime — and kept honest rather than
 * remembered: `test/marks.test.ts` fails when it drifts from the lock, which
 * is the same edit re-vendoring makes anyway.
 */
export const BRAND_RELEASE = "2026.10";

/** How the Mark is laid out. */
export const LOGO_LAYOUTS = ["horizontal", "stacked", "icon", "symbol", "wordmark"] as const;
export type LogoLayout = (typeof LOGO_LAYOUTS)[number];

/**
 * The surface the Mark sits on — the local background, not the global Theme.
 * `light` takes the color Mark, `dark` takes the inverse one.
 */
export const LOGO_SURFACES = ["light", "dark"] as const;
export type LogoSurface = (typeof LOGO_SURFACES)[number];

/** One Mark as the release published it. */
export interface Mark {
  /** File name, exactly as the release names it. */
  file: string;
  /** The URL the bundler emitted for the file. */
  src: string;
  /** The drawing's own width, from its viewBox. */
  width: number;
  /** The drawing's own height, from its viewBox. */
  height: number;
  /**
   * How much of that height the SYMBOL — the red glyph — occupies.
   *
   * The symbol is the same drawing in every layout, which is what makes it the
   * thing to measure: clearspace is 25% of symbol height (ADR 0004), and one
   * minimum on the symbol gives every lockup a minimum without several numbers
   * anyone has to keep in step. In the horizontal, icon and symbol Marks the
   * symbol spans the full height; in the stacked one it sits above the
   * wordmark and occupies 100 of the drawing's 154.423 units. The standalone
   * wordmark has no symbol, so its own 71.4-unit ink box is the sizing
   * reference.
   */
  symbolHeight: number;
}

/**
 * Every Mark the pinned release ships, by layout and surface.
 *
 * The hole is the point: `icon` has no `dark` entry because the release
 * publishes no `reddb-icon-inverse.svg`. It is left absent rather than filled
 * with the color Mark, so the gap is a fact the type system and the refusal
 * both read off one place.
 */
export const MARKS: Readonly<Record<LogoLayout, Partial<Record<LogoSurface, Mark>>>> = {
  horizontal: {
    light: {
      file: "reddb-horizontal-color.svg",
      src: horizontalColor,
      width: 397.712,
      height: 100,
      symbolHeight: 100,
    },
    dark: {
      file: "reddb-horizontal-inverse.svg",
      src: horizontalInverse,
      width: 397.712,
      height: 100,
      symbolHeight: 100,
    },
  },
  stacked: {
    light: {
      file: "reddb-stacked-color.svg",
      src: stackedColor,
      width: 140,
      height: 154.423,
      symbolHeight: 100,
    },
    dark: {
      file: "reddb-stacked-inverse.svg",
      src: stackedInverse,
      width: 140,
      height: 154.423,
      symbolHeight: 100,
    },
  },
  icon: {
    light: {
      file: "reddb-icon-color.svg",
      src: iconColor,
      width: 100,
      height: 100,
      symbolHeight: 100,
    },
    // No `dark`: the pinned release ships no icon inverse. The chatbot avatar
    // on a dark surface is blocked here until the Brand ships one (ADR 0004).
  },
  symbol: {
    // The Brand names these two single-colour variants by their intended
    // grounds: black for light, white for dark. `color` is the primary
    // treatment and `knockout` describes transparency, not a third kind of
    // surface, so neither distorts the `on` vocabulary.
    light: {
      file: "reddb-symbol-black.svg",
      src: symbolBlack,
      width: 100,
      height: 100,
      symbolHeight: 100,
    },
    dark: {
      file: "reddb-symbol-white.svg",
      src: symbolWhite,
      width: 100,
      height: 100,
      symbolHeight: 100,
    },
  },
  wordmark: {
    light: {
      file: "reddb-wordmark-black.svg",
      src: wordmarkBlack,
      width: 308.3,
      height: 71.4,
      symbolHeight: 71.4,
    },
    dark: {
      file: "reddb-wordmark-white.svg",
      src: wordmarkWhite,
      width: 308.3,
      height: 71.4,
      symbolHeight: 71.4,
    },
  },
};

/**
 * Every SVG drawing the pinned release publishes.
 *
 * Most are selected by layout and surface through `MARKS`. The color symbol
 * and knockout symbol are treatments rather than surfaces: they belong in the
 * catalogue and ship byte-identically with the Kit, but do not invent a third
 * `on` value or silently displace the Brand's explicit black/light and
 * white/dark mapping.
 */
export const MARK_CATALOGUE: readonly Mark[] = [
  ...LOGO_LAYOUTS.flatMap((layout) =>
    LOGO_SURFACES.map((on) => MARKS[layout][on]).filter(
      (mark): mark is Mark => mark !== undefined,
    ),
  ),
  {
    file: "reddb-symbol-color.svg",
    src: symbolColor,
    width: 100,
    height: 100,
    symbolHeight: 100,
  },
  {
    file: "reddb-symbol-knockout.svg",
    src: symbolKnockout,
    width: 100,
    height: 100,
    symbolHeight: 100,
  },
];

/**
 * The refusal: the Logo was asked for a Mark the pinned release does not ship.
 *
 * It names the gap and the release, because those are the two things whoever
 * hits it needs — what is missing, and which release it is missing from — and
 * because the fix is upstream: the message is the first draft of the Brand
 * proposal.
 */
export class MarkNotShippedError extends Error {
  /** The layout that was asked for. */
  readonly layout: LogoLayout;
  /** The surface it was asked for. */
  readonly on: LogoSurface;
  /** The Assets release that ships no such Mark. */
  readonly release: string;

  constructor(layout: LogoLayout, on: LogoSurface, release: string = BRAND_RELEASE) {
    const alternatives = LOGO_LAYOUTS.filter((other) => MARKS[other][on] !== undefined);
    super(
      `Logo: the pinned Brand Assets release ${release} ships no ${layout} Mark for a ${on} surface.\n` +
        "The Logo refuses rather than falling back to the other surface's Mark or recoloring one: " +
        "the Brand owns the drawing and the DS owns only its placement (ADR 0004), so a variant " +
        "that does not exist upstream is a Brand proposal, not a local file.\n" +
        `Until the Brand ships it, place the ${layout} Mark on the surface it has ` +
        `(on="${on === "dark" ? "light" : "dark"}"), or use ` +
        `${alternatives.map((other) => `layout="${other}"`).join(" or ")} on a ${on} surface.`,
    );
    this.name = "MarkNotShippedError";
    this.layout = layout;
    this.on = on;
    this.release = release;
  }
}

/**
 * The Mark for one layout on one surface, or a loud refusal when the release
 * ships none. Never falls back, never invents: those are the two failures the
 * licence forbids, so neither has a code path here.
 */
export function selectMark(layout: LogoLayout, on: LogoSurface): Mark {
  const mark = MARKS[layout][on];
  if (mark === undefined) throw new MarkNotShippedError(layout, on);
  return mark;
}

/** Does the pinned release ship a Mark for this layout on this surface? */
export function shipsMark(layout: LogoLayout, on: LogoSurface): boolean {
  return MARKS[layout][on] !== undefined;
}
