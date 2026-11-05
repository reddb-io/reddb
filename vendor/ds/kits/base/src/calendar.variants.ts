// Calendar appearance is token-only; Bits UI owns date state, focus, and announcements.
import { tv, type VariantProps } from "tailwind-variants";

export const calendar = tv({
  slots: {
    root: [
      "w-fit min-w-0 rounded-lg border border-muted bg-background text-foreground",
      "p-[var(--reddb-spatial-inset-md)]",
    ].join(" "),
    header: "mb-[var(--reddb-spatial-gap-sm)] flex items-center justify-between gap-[var(--reddb-spatial-gap-sm)]",
    navigation: "w-[var(--reddb-spatial-control-height-sm)] shrink-0 px-0",
    heading: "min-w-0 text-center text-sm font-medium",
    months: "grid gap-[var(--reddb-spatial-gap-md)]",
    grid: "w-full",
    gridHead: "text-ink-muted",
    gridRow: "grid grid-cols-7",
    headCell: "flex h-[var(--reddb-spatial-control-height-sm)] items-center justify-center text-xs font-regular",
    cell: "p-0 text-center",
    day: [
      "relative inline-flex size-[var(--reddb-spatial-control-height-sm)] items-center justify-center rounded-md",
      "text-sm tabular-nums",
      // Rest, hover, pressed and focus are the quiet-control contract the
      // component wraps this slot in (wave 6A); a chosen day keeps the accent.
      "data-[selected]:bg-primary data-[selected]:font-medium data-[selected]:text-on-primary",
      "data-[range-start]:rounded-e-none data-[range-end]:rounded-s-none",
      "data-[range-middle]:rounded-none data-[range-middle]:border-y data-[range-middle]:border-primary",
      "data-[disabled]:pointer-events-none data-[disabled]:opacity-35",
      "data-[unavailable]:line-through data-[unavailable]:opacity-60",
      "data-[outside-month]:text-ink-muted data-[outside-month]:opacity-50",
      "data-[today]:after:absolute data-[today]:after:bottom-1 data-[today]:after:size-1 data-[today]:after:rounded-full data-[today]:after:bg-current",
    ].join(" "),
  },
});

export type CalendarVariants = VariantProps<typeof calendar>;
