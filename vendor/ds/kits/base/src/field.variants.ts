// Field's public association and appearance seams.
//
// The association object is deliberately control-agnostic: Field can own the
// accessible contract for the canonical Input or for a consumer's explicitly
// named local control without either implementation reaching into the other.

import { tv, type VariantProps } from "tailwind-variants";
import type { FieldLayout } from "./core/field";

export { FIELD_LAYOUTS, type FieldLayout, type FieldControlProps } from "./core/field";

// `layout` decides where the label sits relative to its control. A text-like
// control reads label-first, stacked above it. A binary choice — a checkbox or
// a switch — reads control-first: the box sits at the start of the row, its
// label beside it (and clickable, through the same native `for`), and help and
// error text wrap underneath, aligned with the label text rather than with the
// box. The association contract is identical in both.
const LAYOUT = {
  stacked: { root: "", label: "", help: "", error: "" },
  inline: {
    root: "grid-cols-[auto_minmax(0,1fr)] items-center gap-x-[var(--reddb-spatial-gap-md)]",
    label: "cursor-pointer",
    help: "col-start-2",
    error: "col-start-2",
  },
} as const satisfies Record<FieldLayout, Record<string, string>>;

// A disabled control takes its label down with it: the label steps to
// secondary ink (still AA, WCAG 1.4.3) so a dimmed control never sits beside a
// full-strength name. It follows the control's own `:disabled` — a native
// `disabled` or an enclosing disabled fieldset — and only a control's: a
// disabled <option> inside an enabled Select leaves the label alone. The
// required mark carries no colour of its own, so it steps down with the label.
//
// The error is set in the danger feedback foreground, which the contrast
// contract holds at AA on every ground; the message text, not the colour,
// carries the meaning (WCAG 1.4.1).
export const field = tv({
  slots: {
    root: "group/field grid content-start gap-[var(--reddb-spatial-gap-sm)]",
    label: [
      "text-sm font-medium text-foreground",
      "group-has-[:is(input,select,textarea,button):disabled]/field:cursor-not-allowed",
      "group-has-[:is(input,select,textarea,button):disabled]/field:text-ink-muted",
    ].join(" "),
    required: "ms-[var(--reddb-spatial-gap-sm)]",
    help: "text-sm text-ink-muted",
    error: "text-sm font-medium text-feedback-danger-foreground",
  },
  variants: { layout: LAYOUT },
  defaultVariants: { layout: "stacked" },
});

export type FieldVariants = VariantProps<typeof field>;
