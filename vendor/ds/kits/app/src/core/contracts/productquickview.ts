

/** Portable ProductQuickview properties shared by native adapters. */
export interface ProductQuickviewContract {
/**
     * Accessible name for the Button that opens the dialog, and its visible text when no `trigger`
     * is given.
     */
    triggerLabel: string;
/** Visible dialog title and the dialog's accessible name. */
    title: string;
/** Supporting line under the title, associated with the dialog as its description. */
    description?: string;
/** Extra classes, merged over the dialog slot's own, onto the modal surface. */
    dialogClass?: string;
}
