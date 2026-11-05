import type { SpeedDialAction } from "../../composites/command-surfaces.behavior";
import type { SpeedDialSize } from "../../composites/speed-dial.variants";
type Side = "top" | "right" | "bottom" | "left";
type Align = "start" | "center" | "end";
/** Portable SpeedDial properties shared by native adapters. */
export interface SpeedDialContract {
/** Accessible name for the trigger Button, and its visible text when no `trigger` is given. */
    triggerLabel: string;
/** Accessible name for the revealed actions surface. */
    contentLabel: string;
/** Actions revealed in order; each renders as a button, or as a link when it has an `href`. */
    actions?: readonly SpeedDialAction[];
/** Whether the actions are revealed: `bind:open` (ADR 0026). */
    open?: boolean;
/** Disables the trigger, so the actions cannot be revealed. Defaults to `false`. */
    disabled?: boolean;
/** Size of the trigger and of the action buttons: `sm`, `md` or `lg`. Defaults to `md`. */
    size?: SpeedDialSize;
/**
     * Preferred side of the trigger the actions open on; collision handling may flip it. Defaults
     * to `top`.
     */
    side?: Side;
/** Preferred alignment along the chosen side: `start`, `center` or `end`. Defaults to `end`. */
    align?: Align;
/** Distance, in pixels, between the trigger and the revealed actions. Defaults to `8`. */
    sideOffset?: number;
/**
     * Safe distance, in pixels, kept between the actions and the viewport edges. Defaults to `8`.
     */
    collisionPadding?: number;
}
