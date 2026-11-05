

/** Canonical portable Disclosure properties, shared by native adapters. */
export interface DisclosureContract {
/** Visible text of the trigger Button that toggles the region. */
    label: string;
/** Whether the region is expanded, bindable. Defaults to closed. */
    open?: boolean;
/** Disables the trigger, so the region cannot be toggled. */
    disabled?: boolean;
/** Extra classes merged onto the trigger Button. */
    triggerClass?: string;
/** Extra classes merged onto the expandable region. */
    contentClass?: string;
}
