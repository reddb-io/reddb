import type { ApplicationNavigationItem } from "../../composites/navigation.behavior";

/** Portable SidebarNavigation properties shared by native adapters. */
export interface SidebarNavigationContract {
/** Accessible name for the `nav` landmark. */
    label: string;
/** Destinations, one link per entry in the given order; each `id` must be unique. */
    items: readonly ApplicationNavigationItem[];
/**
     * The `id` of the item for the current page. It must match exactly one entry in `items`, or the
     * component throws.
     */
    currentId: string;
/** Accessible name for the contextual action group. */
    toolbarLabel?: string;
}
