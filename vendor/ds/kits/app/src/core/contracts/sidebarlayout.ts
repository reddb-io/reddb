import type { StackGap } from "@reddb-io/design-system/base";
import type { SidebarSide } from "../../composites/sidebar-layout.variants";
import type { PrimaryNavigationItem } from "../../composites/navigation.behavior";

/** Portable SidebarLayout properties shared by native adapters. */
export interface SidebarLayoutContract {
/** Logical edge occupied by the complementary landmark. */
    side?: SidebarSide;
/** Accessible name that distinguishes this complementary landmark. */
    sidebarLabel?: string;
/**
     * Id of the main region, the skip link's focus target. Defaults to `main-content` while the
     * region is the page's `<main>`; a region inside an ApplicationShell takes no id unless given
     * one, so it never duplicates the shell's.
     */
    mainId?: string;
/**
     * Whether the main region renders the page's `<main>` landmark. Defaults to `true`, or to
     * `false` inside an ApplicationShell, which owns `<main>`; the region is then a plain `div`.
     */
    main?: boolean;
/** Allow primary navigation to become a mobile bottom dock. Defaults to true in navigation mode; false keeps the rail vertical. Workspace rail snippets are unaffected. */
    dockable?: boolean;
/** Primary page sections: href entries navigate, entries without href open sidebar. On wide screens renders a rail, on mobile a bottom bar. Workspace rails can keep using the rail snippet. */
    navigation?: readonly PrimaryNavigationItem[];
/** Current primary destination, owned by the application. Required and unique when navigation is supplied; opening a section never changes it. */
    currentId?: string;
/** Accessible name of the primary navigation. Defaults to Main navigation. */
    navigationLabel?: string;
/** Show each primary rail destination label below its icon or initials. Mobile dock labels remain visible independently. */
    showRailLabels?: boolean;
/** Visible name of the overflow menu when more than five primary items are supplied. Defaults to More. */
    moreLabel?: string;
/** Externally bindable visibility of the compact navigation rail. */
    railOpen?: boolean;
/** Externally bindable visibility of the sidebar panel on mobile. */
    panelOpen?: boolean;
/** Vertical rhythm inside both landmarks. Defaults to the layout tier's `layout-md`. */
    gap?: StackGap;
}
