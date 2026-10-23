type Side = "top" | "right" | "bottom" | "left";
type Align = "start" | "center" | "end";
import type { DateValue } from "@internationalized/date";

/** Canonical portable DateRangePicker properties, shared by native adapters. */
export interface DateRangePickerContract {
/** Visible legend naming the related DateFields. */
    label: string;
/** Visible label for the start DateField. */
    startLabel: string;
/** Visible label for the end DateField. */
    endLabel: string;
/** Accessible name for the RangeCalendar grid and Popover dialog. */
    calendarLabel: string;
/**
     * Accessible name for the Button that opens the calendar. Defaults to `Choose` followed by
     * `label`.
     */
    triggerLabel?: string;
/** Native form name receiving the start date. */
    startName?: string;
/**
     * Native form name receiving the end date; it is not submitted while the end precedes the
     * start.
     */
    endName?: string;
/** Complete ISO bounds shared by DateRangeField and RangeCalendar. */
    startValue?: string;
/** Complete ISO end date, bindable, shared with `startValue` by the field and the calendar. */
    endValue?: string;
/** Supporting text associated with the complete range. */
    help?: string;
/**
     * Message announced when the end precedes the start. Defaults to `End date cannot be before
     * start date.`
     */
    rangeError?: string;
/** Applies native required validation to every segment of both date fields. */
    required?: boolean;
/** Disables both date fields, the trigger Button and the RangeCalendar. */
    disabled?: boolean;
/**
     * Explicit id prefix for the group associations and both date fields. A generated one is used
     * when omitted.
     */
    id?: string;
/** Whether the calendar Popover is open, bindable. Defaults to closed. */
    open?: boolean;
/** Calendar month shown when no range is selected. Defaults to the month of `startValue`. */
    placeholder?: DateValue;
/** Earliest date the RangeCalendar lets the user select. */
    minValue?: DateValue;
/** Latest date the RangeCalendar lets the user select. */
    maxValue?: DateValue;
/** Fewest days a selected range may span. */
    minDays?: number;
/** Most days a selected range may span. */
    maxDays?: number;
/** Called with each date; return `true` to show it in the calendar but make it unselectable. */
    isDateUnavailable?: (date: DateValue) => boolean;
/**
     * Preferred side of the trigger for the Popover; collision handling may flip it. Defaults to
     * `bottom`.
     */
    side?: Side;
/** Preferred alignment of the Popover along the selected side. Defaults to `start`. */
    align?: Align;
/** Distance in pixels between the trigger and the Popover. Defaults to `8`. */
    sideOffset?: number;
/** Safe distance in pixels kept between the Popover and the viewport edges. Defaults to `8`. */
    collisionPadding?: number;
/** Extra classes merged onto the DateRangeField's Fieldset root. */
    fieldClass?: string;
/** Extra classes merged onto each of the two DateField roots. */
    dateFieldClass?: string;
/** Extra classes merged onto all six native date segments. */
    segmentClass?: string;
/** Extra classes merged onto the Button that opens the calendar. */
    triggerClass?: string;
/** Extra classes merged onto the Popover surface that holds the RangeCalendar. */
    contentClass?: string;
/** Extra classes merged onto the RangeCalendar root. */
    calendarClass?: string;
/** Extra classes merged onto the calendar's previous-month and next-month Buttons. */
    navigationClass?: string;
/** Extra classes merged onto each day button in the calendar. */
    dayClass?: string;
}
