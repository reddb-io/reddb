import type { NavigationMenuEntry } from "../../navigation-menu.behavior";
import type { NavigationMenuOrientation, NavigationMenuSize } from "../../navigation-menu.variants";

/** Shared canonical NavigationMenu properties. */
export interface NavigationMenuContract {
/** Accessible name of the navigation landmark. */
    label: string;
/**
     * The top-level entries: a link with an `href` renders a direct destination, one with `links`
     * renders a trigger that opens a surface of links. Defaults to none.
     */
    items?: readonly NavigationMenuEntry[];
/** The `id` of the section whose links are open, or an empty string when none is; bindable. */
    value?: string;
/**
     * Direction the entries run: `horizontal` in a row, `vertical` in a column. Defaults to
     * `horizontal`.
     */
    orientation?: NavigationMenuOrientation;
/**
     * Size of the top-level controls, following the Density control heights: `sm`, `md` or `lg`.
     * Defaults to `md`.
     */
    size?: NavigationMenuSize;
/**
     * Milliseconds from the pointer entering a trigger until its content opens, as Bits UI
     * defines it; defaults to `200`.
     */
    delayDuration?: number;
/**
     * Milliseconds in which moving to another trigger skips the open delay again, as Bits UI
     * defines it; defaults to `300`.
     */
    skipDelayDuration?: number;
}
