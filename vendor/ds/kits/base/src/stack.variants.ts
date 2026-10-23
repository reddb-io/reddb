// Stack's public appearance seam: one flow, in either direction, whose child
// to child rhythm is the Stack's own (ADR 0024) and is drawn from a Density
// role at every tier:
//   - sm | md | lg — the component tier, for the parts of one thing;
//   - layout-sm | layout-md | layout-lg — the layout tier, between fields,
//     stacked groups, and a group's heading and its content;
//   - section — the section tier, between page sections.
// A caller places the Stack; it never re-spaces the Stack's children.
import { tv, type VariantProps } from "tailwind-variants";

const GAP = {
  none: "gap-0",
  sm: "gap-[var(--reddb-spatial-gap-sm)]",
  md: "gap-[var(--reddb-spatial-gap-md)]",
  lg: "gap-[var(--reddb-spatial-gap-lg)]",
  "layout-sm": "gap-[var(--reddb-spatial-layout-gap-sm)]",
  "layout-md": "gap-[var(--reddb-spatial-layout-gap-md)]",
  "layout-lg": "gap-[var(--reddb-spatial-layout-gap-lg)]",
  section: "gap-[var(--reddb-spatial-section-gap)]",
} as const;

/** The flow's main axis. `column` (the default) stacks; `row` runs inline. */
const DIRECTION = {
  column: "flex-col",
  row: "flex-row",
} as const;

/** Cross-axis alignment of the children. `stretch` is the flex default, so it adds no class. */
const ALIGN = {
  stretch: "",
  start: "items-start",
  center: "items-center",
  end: "items-end",
  baseline: "items-baseline",
} as const;

/** Main-axis distribution of the children. `start` is the flex default. */
const JUSTIFY = {
  start: "",
  center: "justify-center",
  end: "justify-end",
  between: "justify-between",
} as const;

/** Whether children wrap onto a new line when the flow runs out of room. */
const WRAP = {
  true: "flex-wrap",
  false: "",
} as const;

export const stack = tv({
  base: "flex",
  variants: { direction: DIRECTION, gap: GAP, align: ALIGN, justify: JUSTIFY, wrap: WRAP },
  defaultVariants: { direction: "column", gap: "layout-md", align: "stretch", justify: "start", wrap: false },
});

export type StackVariants = VariantProps<typeof stack>;
export type StackGap = NonNullable<StackVariants["gap"]>;
export type StackDirection = NonNullable<StackVariants["direction"]>;
export type StackAlign = NonNullable<StackVariants["align"]>;
export type StackJustify = NonNullable<StackVariants["justify"]>;
export const STACK_GAPS = Object.keys(GAP) as readonly StackGap[];
export const STACK_DIRECTIONS = Object.keys(DIRECTION) as readonly StackDirection[];
export const STACK_ALIGNMENTS = Object.keys(ALIGN) as readonly StackAlign[];
export const STACK_JUSTIFICATIONS = Object.keys(JUSTIFY) as readonly StackJustify[];
