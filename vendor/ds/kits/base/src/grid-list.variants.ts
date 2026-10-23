// GridList's columns answer to the width of the list itself, never the window
// (ADR 0021). The list stays the component's root (`<ul>`, which its child
// Kits and their callers address directly), and an element cannot query its
// own size, so the steps are written intrinsically instead: each track's
// minimum is a step function of the list's inline size (`100%` in a grid
// track is the grid's own width) that `auto-fill` turns into a column count.
//
// For `columns={4}` the track minimum is the largest of
//   - a quarter of the row                      (always: 4 columns fit),
//   - half the row,  while narrower than `5xl`  (2 columns),
//   - the whole row, while narrower than `lg`   (1 column),
// where a term `min(…, (step - 100%) * 9999)` is the row-relative size below
// `step` and negative — so ignored by `max()` — from `step` up. The steps are
// the DS's named container sizes: `lg` 32rem, `3xl` 48rem, `5xl` 64rem, so a
// track never narrows below about 15rem. Four columns step 4 → 2 → 1, never
// leaving a single orphan on a second row; three step 3 → 2 → 1; two 2 → 1.
//
// Each term subtracts the gutters at the largest component gap and a pixel of
// slack, so the count is exact at every List gap and never rounds down.
import { tv, type VariantProps } from "tailwind-variants";

export const GRID_LIST_COLUMNS = [1, 2, 3, 4] as const;
export type GridListColumn = (typeof GRID_LIST_COLUMNS)[number];

export const gridList = tv({
  base: "grid w-full",
  variants: {
    columns: {
      1: "grid-cols-1",
      2: "grid-cols-[repeat(auto-fill,minmax(max(calc((100%_-_var(--reddb-spatial-gap-lg))_/_2_-_1px),min(calc(100%_-_1px),calc((var(--reddb-container-lg)_-_100%)*9999))),1fr))]",
      3: "grid-cols-[repeat(auto-fill,minmax(max(calc((100%_-_2*var(--reddb-spatial-gap-lg))_/_3_-_1px),min(calc((100%_-_var(--reddb-spatial-gap-lg))_/_2_-_1px),calc((var(--reddb-container-3xl)_-_100%)*9999)),min(calc(100%_-_1px),calc((var(--reddb-container-lg)_-_100%)*9999))),1fr))]",
      4: "grid-cols-[repeat(auto-fill,minmax(max(calc((100%_-_3*var(--reddb-spatial-gap-lg))_/_4_-_1px),min(calc((100%_-_var(--reddb-spatial-gap-lg))_/_2_-_1px),calc((var(--reddb-container-5xl)_-_100%)*9999)),min(calc(100%_-_1px),calc((var(--reddb-container-lg)_-_100%)*9999))),1fr))]",
    },
  },
  defaultVariants: { columns: 1 },
});

export type GridListVariants = VariantProps<typeof gridList>;
