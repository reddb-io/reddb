

/** Canonical portable TimeField properties, shared by native adapters. */
export interface TimeFieldContract {
/** Visible label text owned by the caller. */
    label: string;
/** Native form name receiving a complete, possible time. */
    name?: string;
/** Complete 24-hour clock value. */
    value?: string;
/** Supporting text associated with both segments. */
    help?: string;
/** Current validation error associated with both segments. */
    error?: string;
/** Applies native required validation to both segments. */
    required?: boolean;
/** Disables both native segments. */
    disabled?: boolean;
/** Explicit id for the hour segment. */
    id?: string;
/** Extra classes merged onto both native segments. */
    segmentClass?: string;
}
