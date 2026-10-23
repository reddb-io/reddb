import type { MenubarMenu } from "../../composites/command-surfaces.behavior";
import type { MenubarSize } from "../../composites/menubar.variants";

/** Portable Menubar properties shared by native adapters. */
export interface MenubarContract {
/** Accessible name for the menubar. */
    label: string;
/**
     * Top-level menus in order; each has an `id`, a trigger `label` and its `items`, and may be
     * `disabled`.
     */
    menus?: readonly MenubarMenu[];
/** The open menu's id, `""` when none is open: `bind:value` (ADR 0026). */
    value?: string;
/** Size of the trigger buttons and the menu rows: `sm`, `md` or `lg`. Defaults to `md`. */
    size?: MenubarSize;
/**
     * Whether arrow-key focus wraps from the last menu back to the first, and the reverse. Defaults
     * to `true`.
     */
    loop?: boolean;
/** Distance, in pixels, between an open menu and its trigger. Defaults to `8`. */
    sideOffset?: number;
/**
     * Safe distance, in pixels, kept between an open menu and the viewport edges. Defaults to `8`.
     */
    collisionPadding?: number;
}
