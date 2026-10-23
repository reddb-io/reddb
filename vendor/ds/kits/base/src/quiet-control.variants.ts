// The interaction-state contract every quiet control draws (wave 6A).
//
// A quiet control is anything a person can press that carries no fill and no
// boundary of its own at rest: a ghost Button, an icon-only button, a close or
// dismiss "×", a tab, an accordion trigger, a navigation item, a pager, a
// carousel or deck dot, a sort header, a tree toggle. Before this recipe each
// drew its own states, and about half of them changed nothing but the text
// colour on hover. Now every one of them answers the pointer the same way:
//
// - rest: transparent, so it sits on whatever ground it lands on;
// - hover: the quiet fill, `bg-foreground/8`. Tailwind 4 compiles `hover:`
//   inside `@media (hover: hover)`, so a touch screen never keeps a sticky
//   fill after a tap;
// - pressed: the stronger quiet fill, `bg-foreground/12` — the touch screen's
//   only feedback, so it must be stronger than hover, not equal to it. A
//   trigger holding its popup open (`aria-haspopup` + `aria-expanded`) wears
//   the open fill, `bg-foreground/10`;
// - focus: the ink outline every control draws (ADR 0023), outside the control
//   by default and inside it (`focus: "inset"`) where a scroll region would
//   clip one drawn outside;
// - disabled: half opacity and no pointer, Button's treatment;
// - selected: the selection language (DESIGN.md, "Interactive states"): the
//   selected surface `bg-foreground/10`, full-weight ink, and — where the
//   component draws one — the 2px ink edge. A selected item does not lighten
//   under the pointer.
//
// Button's ghost variant is this recipe. A component whose element is not a
// Button wraps its own slot classes in it, `quietControl({ class: … })`, so the
// slot only adds geometry and the states stay here. kit-lint's rule 8 checks
// that every raw interactive element in a Kit does one or the other.

import { tv, type VariantProps } from "tailwind-variants";
import { typeRoleMerge } from "./type-roles";

/** The quiet fill under the pointer. */
export const QUIET_HOVER = "hover:bg-foreground/8";
/** The stronger quiet fill while pressed: also the only feedback on touch. */
export const QUIET_PRESSED = "active:bg-foreground/12";
/** A trigger holding its popup open. */
export const QUIET_OPEN = "[&[aria-haspopup][aria-expanded=true]]:bg-foreground/10";
/** The ink focus outline (ADR 0023), drawn outside the control. */
export const QUIET_FOCUS = "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus";
/** Button's disabled treatment, for a native `disabled` or `aria-disabled`. */
export const QUIET_DISABLED =
  "disabled:pointer-events-none disabled:opacity-50 aria-disabled:pointer-events-none aria-disabled:opacity-50";

const INK = {
  /** Secondary ink at rest, full ink under the pointer: a ghost Button, a close "×". */
  muted: "text-ink-muted hover:text-foreground",
  /** Full ink throughout: a menu trigger or a row whose label is the content. */
  foreground: "text-foreground",
  /** The caller's colour (a tone, the surrounding text). */
  inherit: "",
} as const;

const FOCUS = {
  /** The default: 2px outside the control, on the ground. */
  offset: QUIET_FOCUS,
  /** Inside the control, where a scroll region or a tight rail would clip it. */
  inset: "focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-focus",
} as const;

const SELECTED = {
  /** Current, selected or pressed: the selected surface, which the pointer does not lighten. */
  true: "bg-foreground/10 hover:bg-foreground/10 font-medium text-foreground hover:text-foreground",
  false: "",
} as const;

export const quietControl = tv({
  base: [
    // No colour transition: a state lands at full contrast the moment it
    // starts. Easing colours faded the focus ring in from the text colour, and
    // made a Color Scheme switch repaint every control through intermediate
    // colours that fail AA for 150ms.
    "bg-transparent",
    QUIET_HOVER,
    QUIET_PRESSED,
    QUIET_OPEN,
    QUIET_DISABLED,
  ].join(" "),
  variants: { ink: INK, focus: FOCUS, selected: SELECTED },
  defaultVariants: { ink: "muted", focus: "offset", selected: false },
}, typeRoleMerge);

export type QuietControlVariants = VariantProps<typeof quietControl>;
export type QuietControlInk = NonNullable<QuietControlVariants["ink"]>;
export type QuietControlFocus = NonNullable<QuietControlVariants["focus"]>;

export const QUIET_CONTROL_INKS = Object.keys(INK) as readonly QuietControlInk[];
export const QUIET_CONTROL_FOCUS = Object.keys(FOCUS) as readonly QuietControlFocus[];
