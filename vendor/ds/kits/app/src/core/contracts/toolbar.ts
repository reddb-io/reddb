import type { ToolbarItem } from "../../composites/command-surfaces.behavior";
import type { ToolbarSize } from "../../composites/toolbar.variants";
type Orientation = "horizontal" | "vertical";
/** Portable Toolbar properties shared by native adapters. */
export interface ToolbarContract {
/** Accessible name for the toolbar. */
    label: string;
/**
     * Actions and destinations in order; an enabled item with an `href` renders as a link, any
     * other as a button that calls its `onselect`.
     */
    items?: readonly ToolbarItem[];
/**
     * Layout direction and the arrow-key axis: `horizontal` or `vertical`. Defaults to
     * `horizontal`.
     */
    orientation?: Orientation;
/** Size of the item controls: `sm`, `md` or `lg`. Defaults to `md`. */
    size?: ToolbarSize;
/**
     * Whether arrow-key focus wraps from the last item back to the first, and the reverse. Defaults
     * to `true`.
     */
    loop?: boolean;
}
