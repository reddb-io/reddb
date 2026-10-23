

/** Canonical portable TimeRangeField properties, shared by native adapters. */
export interface TimeRangeFieldContract {
/** Visible legend naming the related bounds. */
    label: string;
/** Visible label for the first TimeField. */
    startLabel: string;
/** Visible label for the second TimeField. */
    endLabel: string;
/** Native form name receiving the ordered start value. */
    startName?: string;
/** Native form name receiving the ordered end value. */
    endName?: string;
/** Complete start clock value. */
    startValue?: string;
/** Complete end clock value. */
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
/** Extra classes merged onto each TimeField root. */
    fieldClass?: string;
/** Extra classes merged onto all four time segments. */
    segmentClass?: string;
}
