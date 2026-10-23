import { tv, type VariantProps } from "tailwind-variants";

// Container-first (ADR 0021): the layout is a named size container, and its
// columns step against the width it was given, never the window's — so a
// MultiColumnLayout in a pane or beside a sidebar stacks where it is narrow.
// The root cannot query its own width, so the steps are on the columns: a
// flex-wrap row whose columns are a full line each, until the container is
// wide enough for them to share one line in proportion (1 : 2, then 1 : 2 : 1).
// The gutter between columns is the Density layout tier (ADR 0024). A size
// container takes its width from its parent, never its content, so the root
// is `w-full`: inside a shrink-to-fit parent it would otherwise collapse.
const COLUMNS = {
  two: {
    start: "@3xl/multi-column-layout:grow-[1] @3xl/multi-column-layout:basis-0",
    main: "@3xl/multi-column-layout:grow-[2] @3xl/multi-column-layout:basis-0",
    end: "",
  },
  three: {
    start: "@3xl/multi-column-layout:grow-[1] @3xl/multi-column-layout:basis-0",
    main: "@3xl/multi-column-layout:grow-[2] @3xl/multi-column-layout:basis-0",
    end: "@5xl/multi-column-layout:grow-[1] @5xl/multi-column-layout:basis-0",
  },
} as const;

/** Two or three columns that answer to their own container, with no assumptions about their content. */
export const multiColumnLayout = tv({
  slots: {
    root: "@container/multi-column-layout flex w-full min-w-0 flex-wrap items-start gap-[var(--reddb-spatial-layout-gap-md)]",
    column: "min-w-0 basis-full",
    start: "",
    main: "",
    end: "",
  },
  variants: { columns: COLUMNS },
  defaultVariants: { columns: "two" },
});

export type MultiColumnLayoutVariants = VariantProps<typeof multiColumnLayout>;
export type MultiColumnCount = NonNullable<MultiColumnLayoutVariants["columns"]>;
