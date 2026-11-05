import { tv, type VariantProps } from "tailwind-variants";

// Container-first (ADR 0021): the two columns answer to the overview's own
// width, never the window's. The root is the grid, and an element cannot
// query its own size, so the step is intrinsic, as Base GridList's: each
// track's minimum is half the row (two columns) from the DS's `3xl` container
// step (48rem) up, and the whole row below it (one column). The gutter is the
// Density layout tier (ADR 0024), and the step subtracts that gutter, so the
// count is exact at every stop.

export const productOverview = tv({
  slots: {
    root: "grid min-w-0 items-start gap-[var(--reddb-spatial-layout-gap-md)] grid-cols-[repeat(auto-fill,minmax(max(calc((100%_-_var(--reddb-spatial-layout-gap-md))_/_2_-_1px),min(calc(100%_-_1px),calc((var(--reddb-container-3xl)_-_100%)*9999))),1fr))]",
    media: "min-w-0 overflow-hidden rounded-lg bg-muted",
    content: "min-w-0",
    heading: "items-start pb-0",
    price: "text-lg font-medium text-foreground",
    body: "flex flex-col gap-[var(--reddb-spatial-gap-md)] text-foreground",
    actions: "flex flex-wrap items-center gap-[var(--reddb-spatial-gap-md)]",
  },
});

export type ProductOverviewVariants = VariantProps<typeof productOverview>;
