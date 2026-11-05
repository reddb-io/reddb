

/** Canonical portable DateRangeField properties, shared by native adapters. */
export interface DateRangeFieldContract {
/** Visible legend naming the related bounds. */
    label: string;
/** Visible label for the first DateField. */
    startLabel: string;
/** Visible label for the second DateField. */
    endLabel: string;
/** Native form name receiving the ordered start value. */
    startName?: string;
/** Native form name receiving the ordered end value. */
    endName?: string;
/** Complete start calendar date. */
    startValue?: string;
/** Complete end calendar date. */
    endValue?: string;
/** Supporting text associated with the complete range. */
    help?: string;
/** Caller-owned message used when the end precedes the start. */
    rangeError?: string;
/** Applies native required validation to every segment. */
    required?: boolean;
/** Disables the native group and both composed fields. */
    disabled?: boolean;
/** Explicit id prefix for group associations and both fields. */
    id?: string;
/** Extra classes merged onto each DateField root. */
    fieldClass?: string;
/** Extra classes merged onto all six date segments. */
    segmentClass?: string;
}
