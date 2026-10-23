import type { PrimaryNavigationItem } from "../../composites/navigation.behavior";

interface BottomNavigationItem extends PrimaryNavigationItem {
    /** Whether this destination's contextual menu is open, independent from currentId. */
    expanded?: boolean;
    /** Id of the contextual menu controlled by this item. */
    controls?: string;
  }
/** Portable BottomNavigation properties shared by native adapters. */
export interface BottomNavigationContract {
/** Accessible name for the `nav` landmark. */
    label: string;
/** Destinations, native links or section buttons in the given order; each `id` must be unique. */
    items: readonly BottomNavigationItem[];
/**
     * The `id` of the item for the current page. It must match exactly one entry in `items`, or the
     * component throws.
     */
    currentId: string;
}
