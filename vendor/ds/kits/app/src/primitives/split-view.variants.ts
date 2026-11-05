// SplitView's styling. See button.variants.ts for the split, the colour rule
// and the spatial rule.
//
// The pane sizes are NOT here, and cannot be: the divider's position is a
// runtime number, so it reaches the element as an inline `flex-basis` rather
// than as a class. That is not a hardcoded value in the sense the Kit's lint
// cares about — it carries no Brand decision, only where the user last let go
// of the divider — and there is no class name that could express it.
//
// It follows that this is the one component in the Kit with nothing for the
// Density axis to re-resolve. Everything spatial about a SplitView is either
// that runtime fraction or the divider's own thickness, and a rule's thickness
// is a width — a dimension the Brand ships no token family for at all, which is
// the same reason nav-item.variants.ts draws its active state without an accent
// bar. A stop still reaches the panes' contents, which is where the space a
// reader actually sees lives.

import { tv, type VariantProps } from "tailwind-variants";

const ORIENTATION = {
  /** Panes side by side, divided by a vertical rule. */
  // The rule stays 4px; a transparent band around it takes the pointer, so the
  // drag target is 24px across (WCAG 2.2 SC 2.5.8).
  horizontal: {
    root: "flex-row",
    divider: "relative w-1 cursor-col-resize before:absolute before:inset-y-0 before:-inset-x-2.5 before:content-['']",
  },
  /** Panes stacked, divided by a horizontal one. */
  vertical: {
    root: "flex-col",
    divider: "relative h-1 cursor-row-resize before:absolute before:inset-x-0 before:-inset-y-2.5 before:content-['']",
  },
} as const;

const DRAGGING = {
  /** While the divider is held: the rule takes the accent it is being moved by. */
  true: { divider: "bg-primary" },
  false: { divider: "bg-muted hover:bg-control-edge" },
} as const;

export const splitView = tv({
  slots: {
    root: "flex w-full items-stretch overflow-hidden",
    // `min-w-0`/`min-h-0`: without them a flex item refuses to shrink below
    // its content, and the divider stops halfway through a drag for reasons
    // nothing on screen explains.
    //
    // Each pane is a named size container (ADR 0021): what it holds lays out
    // against the pane it was given, not the window, so a GridList in a narrow
    // pane steps down even on a wide screen.
    pane: "@container/split-pane min-h-0 min-w-0 grow-0 overflow-auto",
    // Focus is the ink focus outline every DS control draws (ADR 0023).
    divider:
      "shrink-0 touch-none select-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus",
  },
  variants: { orientation: ORIENTATION, dragging: DRAGGING },
  defaultVariants: { orientation: "horizontal", dragging: false },
});

export type SplitViewVariants = VariantProps<typeof splitView>;
