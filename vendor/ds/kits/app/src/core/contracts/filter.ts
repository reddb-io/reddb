import type { FilterOption } from "../../composites/filter.variants";

/** Portable Filter properties shared by native adapters. */
export interface FilterContract {
/** Visible name for the related filter choices. */
    legend: string;
/** Caller-owned choices; exactly one is announced as pressed. */
    options: readonly FilterOption[];
/** Active value; an absent value selects the first enabled option. */
    value?: string;
/** Optional native form name for the active value. */
    name?: string;
/** Disables the whole group and every option. Defaults to `false`. */
    disabled?: boolean;
/**
     * Extra classes, merged over the list slot's own, onto the element that wraps the option
     * buttons.
     */
    listClass?: string;
/** Extra classes merged onto each option button. */
    optionClass?: string;
}
