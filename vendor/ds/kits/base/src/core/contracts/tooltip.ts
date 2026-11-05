

/** Canonical portable Tooltip properties, shared by native adapters. */
export interface TooltipContract {
/**
     * Accessible name of the DS-rendered trigger; it remains complete when the
     * Tooltip is unavailable. Required unless `child` supplies the trigger.
     */
    label?: string;
/** Supplemental description shown by the Tooltip. */
    content: string;
}
