import type { ContextMenuEntry } from "../../composites/command-surfaces.behavior";
import type { ContextMenuSize } from "../../composites/context-menu.variants";

/** Portable ContextMenu properties shared by native adapters. */
export interface ContextMenuContract {
/**
     * Accessible name for the trigger Button, and its visible text when no `children` are given.
     */
    triggerLabel: string;
/** Accessible name for the opened menu surface. */
    contentLabel: string;
/**
     * Menu rows and groups in display order; a group's `heading` names it and groups are divided by
     * a separator.
     */
    items?: readonly ContextMenuEntry[];
/** Whether the menu is open: `bind:open` (ADR 0026). */
    open?: boolean;
/**
     * Disables the trigger, so the menu cannot be opened by pointer or keyboard. Defaults to
     * `false`.
     */
    disabled?: boolean;
/** Size of the trigger and of the menu rows: `sm`, `md` or `lg`. Defaults to `md`. */
    size?: ContextMenuSize;
/** Safe distance, in pixels, kept between the menu and the viewport edges. Defaults to `8`. */
    collisionPadding?: number;
/** Extra classes merged onto the trigger Button, the element that opens the menu. */
    targetClass?: string;
}
