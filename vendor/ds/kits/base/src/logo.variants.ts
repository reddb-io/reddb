// The Logo's styling, and nothing else.
//
// Every class the component wears lives here, in one `tv()` call, by the same
// convention the application Kit follows: the component file holds behavior
// only, and the anti-hardcode lint has a single place to read a component's
// whole visual vocabulary.
//
// There is very little to say, and that is the point. The Logo shows a Brand
// drawing the DS may not recolor (ADR 0004), so it names no colour for the
// Mark at all — the one colour here is the focus ring's, which belongs to the
// application's surface and not to the Brand's drawing, and is named through
// the Theme Layer's utility like every other colour in a Kit.
//
// Mark geometry remains absent: clearspace and size are arithmetic on the
// caller's `size` (logo.box.ts), lengths no Theme or Density stop may reassign.
// Density instead owns the separation outside that licensed box, so a compact
// masthead and a spacious one can place the same unmodified Logo honestly.

import { tv, type VariantProps } from "tailwind-variants";

const INTERACTIVE = {
  /**
   * The `href` form. The ink focus outline (ADR 0023) is drawn on this
   * element — the box that already INCLUDES the clearspace — so it falls
   * outside the clearspace by construction; its 2px offset only moves it
   * further out.
   */
  true: "rounded-md focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus",
  /** The default: the pure Mark, in a footer or as an avatar. */
  false: "",
} as const;

export const logo = tv({
  // `inline-flex` so the box is exactly the Mark plus its clearspace and no
  // line-height creeps in underneath; `shrink-0` so a flex parent cannot
  // squeeze the clearspace away, which would be the one opt-out ADR 0004 says
  // there is none of.
  base: [
    "m-[var(--reddb-spatial-gap-sm)]",
    "inline-flex shrink-0 items-center justify-center",
  ].join(" "),
  variants: { interactive: INTERACTIVE },
  defaultVariants: { interactive: false },
});

/**
 * The Mark itself. `block` because an inline image sits on a text baseline and
 * would push the clearspace out of true; nothing else, because everything the
 * drawing looks like is the Brand's.
 */
export const logoMark = tv({ base: "block" });

export type LogoVariants = VariantProps<typeof logo>;
