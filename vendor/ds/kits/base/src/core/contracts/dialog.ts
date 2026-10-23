import type { DialogSize } from "../../dialog.variants";

/** Canonical portable Dialog properties, shared by native adapters. */
export interface DialogContract {
/**
     * Accessible name for the DS-owned trigger. Omit it to render no trigger
     * and open the Dialog through `bind:open` instead.
     */
    triggerLabel?: string;
/** Visible Dialog title and its accessible name. */
    title: string;
/** Optional visible detail associated with the Dialog. */
    description?: string;
/** Where focus enters when the Dialog opens. Defaults to its body. */
    initialFocus?: "body" | "actions";
/** Whether to render the standard close control. Defaults to true. */
    showClose?: boolean;
/** Whether the Dialog is open. Bindable; defaults to closed. */
    open?: boolean;
/** Whether a click outside the surface dismisses it. Defaults to true. */
    dismissible?: boolean;
/** Maximum inline size of the surface. Defaults to `md`. */
    size?: DialogSize;
}
