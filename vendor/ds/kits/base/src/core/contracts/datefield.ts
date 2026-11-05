

/** Canonical portable DateField properties, shared by native adapters. */
export interface DateFieldContract {
/** Visible label text owned by the caller. */
    label: string;
/** Native form name receiving a complete, possible date. */
    name?: string;
/** Complete ISO calendar date. */
    value?: string;
/** Supporting text associated with every segment. */
    help?: string;
/** Current validation error associated with every segment. */
    error?: string;
/** Applies native required validation to every segment. */
    required?: boolean;
/** Disables every native segment. */
    disabled?: boolean;
/** Explicit id for the year segment. */
    id?: string;
/** Extra classes merged onto every native segment. */
    segmentClass?: string;
}
