import type { CommandPaletteCommand } from "../../composites/command-palette.behavior";

/** Portable CommandPalette properties shared by native adapters. */
export interface CommandPaletteContract {
/** Accessible name and visible fallback for the invoking Button. */
    triggerLabel: string;
/** Accessible name for the Popover dialog. */
    contentLabel: string;
/** Visible Field label for command search. */
    label: string;
/** Caller-owned command names and business actions. */
    commands?: readonly CommandPaletteCommand[];
/** Whether the palette is open: `bind:open` (ADR 0026). */
    open?: boolean;
/** Disables the trigger Button, so the palette cannot be opened. Defaults to `false`. */
    disabled?: boolean;
/**
     * Extra classes, merged over the content slot's own, onto the Popover surface that holds the
     * search.
     */
    contentClass?: string;
/** Extra classes merged onto the command search input. */
    inputClass?: string;
}
