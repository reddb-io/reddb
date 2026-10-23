
type CurrentToken = "page" | "step" | "location" | "date" | "time" | "true";
/** Portable NavItem properties shared by native adapters. */
export interface NavItemContract {
/** The item's text. Ignored when a `children` snippet is given. */
    label?: string;
/** Where the item goes. Absent, the item is a <button>. */
    href?: string;
/**
     * Is this the current destination (ADR 0026)? `true` announces it as the
     * current `page`; a token names what it is current within — `step`
     * inside a wizard, `location` inside a breadcrumb trail. Defaults to false.
     */
    current?: boolean | CurrentToken;
/** Unavailable for now: dimmed, and out of the tab order. */
    disabled?: boolean;
}
