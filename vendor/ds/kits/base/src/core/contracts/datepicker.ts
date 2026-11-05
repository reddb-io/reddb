type Side = "top" | "right" | "bottom" | "left";
type Align = "start" | "center" | "end";
import type { DateValue } from "@internationalized/date";

/** Canonical portable DatePicker properties, shared by native adapters. */
export interface DatePickerContract {
/** Visible DateField label. */
    label: string;
/** Accessible name for the Calendar grid and Popover dialog. */
    calendarLabel: string;
/** Accessible name for the canonical Button that opens the calendar. */
    triggerLabel?: string;
/** Native form name receiving the synchronized complete date. */
    name?: string;
/** Complete ISO calendar date shared by DateField and Calendar. */
    value?: string;
/** Supporting text associated with every date segment. */
    help?: string;
/** Current validation error associated with every date segment. */
    error?: string;
/** Applies native required validation to every date segment and marks the label as required. */
    required?: boolean;
/** Disables the date segments, the trigger Button and the Calendar. */
    disabled?: boolean;
/** Explicit id for the year segment. A generated one is used when omitted. */
    id?: string;
/** Whether the calendar Popover is open. */
    open?: boolean;
/** Calendar month shown when no value is selected. */
    placeholder?: DateValue;
/** Earliest date the Calendar lets the user select. */
    minValue?: DateValue;
/** Latest date the Calendar lets the user select. */
    maxValue?: DateValue;
/** Called with each date; return `true` to show it in the Calendar but make it unselectable. */
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
/** Extra classes merged onto the DateField root. */
    fieldClass?: string;
/** Extra classes merged onto each native date segment. */
    segmentClass?: string;
/** Extra classes merged onto the Button that opens the calendar. */
    triggerClass?: string;
/** Extra classes merged onto the Popover surface that holds the Calendar. */
    contentClass?: string;
/** Extra classes merged onto the Calendar root. */
    calendarClass?: string;
/** Extra classes merged onto the Calendar's previous-month and next-month Buttons. */
    navigationClass?: string;
/** Extra classes merged onto each day button in the Calendar. */
    dayClass?: string;
}
