import type { NavbarLink, NavbarCollapse, NavbarAlign } from "../navbar";

/** Shared canonical Navbar properties. */
export interface NavbarContract {
/**
     * The destinations, in order; shown in the rail and repeated in the compact menu panel. Each
     * entry with an `href` renders a link, one without renders a ghost Button that calls its
     * `onselect`.
     */
    links?: readonly NavbarLink[];
/**
     * How the bar arranges itself: `responsive` follows the bar's own width, `expanded` always
     * shows the rail, `collapsed` always shows the compact menu. Defaults to `responsive`.
     */
    collapse?: NavbarCollapse;
/**
     * Where the links sit between the brand and the actions: `start` keeps them beside the brand,
     * `center` centers them. Defaults to `start`.
     */
    align?: NavbarAlign;
/**
     * Whether the compact menu panel is open; bindable. Escape closes it and returns focus to the
     * menu button.
     */
    open?: boolean;
/** Accessible name of the navigation landmark; defaults to `Main`. */
    label?: string;
/** Text and accessible name of the button that opens the compact menu; defaults to `Menu`. */
    menuLabel?: string;
}
