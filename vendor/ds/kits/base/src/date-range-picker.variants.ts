// DateRangePicker composes DateRangeField, Popover, and RangeCalendar. This
// seam owns only their arrangement and removes the redundant calendar frame.

import { tv, type VariantProps } from "tailwind-variants";

export const dateRangePicker = tv({
  slots: {
    root: "flex min-w-0 items-end gap-[var(--reddb-spatial-gap-sm)]",
    field: "min-w-0 flex-1",
    trigger: "shrink-0",
    calendar: "border-0 p-0",
  },
});

export type DateRangePickerVariants = VariantProps<typeof dateRangePicker>;
