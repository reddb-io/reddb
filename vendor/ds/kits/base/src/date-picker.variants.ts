// DatePicker composes DateField, Popover, and Calendar appearance. This seam
// owns only their arrangement and removes the redundant nested Calendar frame.

import { tv, type VariantProps } from "tailwind-variants";

export const datePicker = tv({
  slots: {
    root: "flex min-w-0 items-end gap-[var(--reddb-spatial-gap-sm)]",
    field: "min-w-0 flex-1",
    trigger: "shrink-0",
    content: "w-fit max-w-none",
    calendar: "border-0 p-0",
  },
});

export type DatePickerVariants = VariantProps<typeof datePicker>;
