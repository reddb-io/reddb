import type { NavbarCollapse } from "@reddb-io/design-system/base";
import type { ApplicationNavigationItem } from "../../composites/navigation.behavior";

/** Portable StoreNavigation properties shared by native adapters. */
export interface StoreNavigationContract {
/** Accessible name for the navbar's `nav` landmark. */
    label: string;
/** Destinations, one link per entry in the given order; each `id` must be unique. */
    items: readonly ApplicationNavigationItem[];
/**
     * The `id` of the item for the current page. It must match exactly one entry in `items`, or the
     * component throws.
     */
    currentId: string;
/**
     * How the links fold into a menu: `responsive` (by the navbar's own width), `expanded` or
     * `collapsed`. Defaults to `responsive`.
     */
    collapse?: NavbarCollapse;
}
