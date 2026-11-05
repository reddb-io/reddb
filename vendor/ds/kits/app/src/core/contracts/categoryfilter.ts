import type { CategoryFilterOption } from "../../composites/category-filter.variants";

/** Portable CategoryFilter properties shared by native adapters. */
export interface CategoryFilterContract {
/** Visible name for the category choices. */
    legend: string;
/** Caller-owned categories rendered as native checkbox choices. */
    options: readonly CategoryFilterOption[];
/** Active category values: `bind:value` (ADR 0026). */
    value?: string[];
/** Optional native form name shared by every category. */
    name?: string;
/** Disables the whole fieldset and every category checkbox. Defaults to `false`. */
    disabled?: boolean;
/**
     * Extra classes, merged over the list slot's own, onto the element that wraps the checkboxes.
     */
    listClass?: string;
/** Extra classes, merged over the option slot's own, onto each checkbox's field wrapper. */
    optionClass?: string;
}
