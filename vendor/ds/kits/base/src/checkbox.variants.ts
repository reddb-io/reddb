// Checkbox's appearance seam. The native input keeps checked, required,
// disabled, focus and form behavior; this seam only draws it.
//
// A native checkbox paints itself (`appearance: auto`), so a border, a fill or
// an invalid edge written on it never renders. The input is therefore the
// pointer target only — a control-height square, never under WCAG 2.2's 24px
// (SC 2.5.8) — and draws nothing; the two spans after it draw the choice from
// tokens and follow its state through `peer-*`: the edge, the checked or mixed
// fill, the invalid edge, the focus ring. The drawn box is a glyph, sized by
// the icon role, so a spacious stop gives it room instead of ballooning it.
// The check is two token-coloured strokes, not an icon: a Base component never
// imports a glyph (ADR 0015), exactly as the Switch's thumb is drawn. A mixed
// (`indeterminate`) checkbox draws a dash instead, as the native one did, and
// the dash wins over a check exactly as it does natively.
//
// Forced colors repaint backgrounds, so there the drawing steps aside and the
// platform's own checkbox comes back — with its focus outline: `outline-hidden`
// is no outline at all until forced colors, where it becomes a system-coloured
// one.

import { tv, type VariantProps } from "tailwind-variants";

/**
 * The drawn box every native choice shares: Checkbox squares it and adds the
 * mixed fill, RadioGroup rounds it. The mixed fill stays Checkbox's alone — a
 * radio matches `:indeterminate` whenever its group has no choice yet.
 */
export const choiceBox = [
  "pointer-events-none col-start-1 row-start-1 size-[var(--reddb-spatial-icon-size-md)]",
  "border border-control-edge bg-background",
  "peer-checked:border-primary peer-checked:bg-primary",
  "peer-aria-invalid:border-feedback-danger-border",
  "peer-focus-visible:outline-2 peer-focus-visible:outline-offset-2 peer-focus-visible:outline-focus",
  "forced-colors:hidden",
].join(" ");

export const checkbox = tv({
  slots: {
    root: [
      "inline-grid size-[var(--reddb-spatial-control-height-sm)] shrink-0 place-items-center",
      "has-disabled:opacity-50",
    ].join(" "),
    control: [
      "peer col-start-1 row-start-1 m-0 size-full cursor-pointer appearance-none",
      "focus-visible:outline-hidden disabled:cursor-not-allowed forced-colors:appearance-auto",
    ].join(" "),
    box: [choiceBox, "rounded-sm peer-indeterminate:border-primary peer-indeterminate:bg-primary"].join(" "),
    // A short and a long stroke at right angles, turned 45°: the box's centre
    // sits a tenth of the glyph below the check's, so it rises by as much.
    // Mixed, the long stroke alone lies flat across the middle: a dash.
    mark: [
      "pointer-events-none invisible col-start-1 row-start-1 peer-checked:visible",
      "aspect-[1/2] w-[calc(var(--reddb-spatial-icon-size-md)*0.3)]",
      "-translate-y-[calc(var(--reddb-spatial-icon-size-md)*0.1)] rotate-45",
      "border-r-2 border-b-2 border-on-primary",
      "peer-indeterminate:visible peer-indeterminate:aspect-auto peer-indeterminate:h-0",
      "peer-indeterminate:w-[calc(var(--reddb-spatial-icon-size-md)*0.5)]",
      "peer-indeterminate:translate-y-0 peer-indeterminate:rotate-0 peer-indeterminate:border-r-0",
      "forced-colors:hidden",
    ].join(" "),
  },
});

export type CheckboxVariants = VariantProps<typeof checkbox>;
