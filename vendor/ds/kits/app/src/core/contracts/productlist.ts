import type { GridListColumn } from "@reddb-io/design-system/base";
import type { ProductListItem } from "../product-list";

/** Portable ProductList properties shared by native adapters. */
export interface ProductListContract {
/** Required accessible name for the product collection. */
    label: string;
/** Product content remains consumer-owned and is rendered in the given order. */
    items: readonly ProductListItem[];
/** Number of grid columns for the products: `1`, `2`, `3` or `4`. Defaults to `2`. */
    columns?: GridListColumn;
/**
     * Outline level of each product name, `2` to `6` (default `3`). Set it one below the
     * heading that introduces the collection so the page outline never skips a level.
     */
    headingLevel?: 2 | 3 | 4 | 5 | 6;
}
