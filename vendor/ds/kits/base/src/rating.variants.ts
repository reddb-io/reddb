// Rating's option shape and token-only appearance seam. The radios are visually
// hidden, so the marks carry their state: focus, disabled, and the danger
// foreground while the group has an error.
import { tv, type VariantProps } from "tailwind-variants";

export const rating = tv({
  slots: {
    root: "min-w-0",
    list: "flex flex-wrap items-center gap-[var(--reddb-spatial-gap-sm)]",
    option: "relative cursor-pointer text-foreground has-disabled:cursor-not-allowed",
    control: "peer sr-only",
    mark: [
      "inline-flex h-[var(--reddb-spatial-control-height-sm)] min-w-[var(--reddb-spatial-control-height-sm)]",
      "items-center justify-center text-foreground peer-aria-invalid:text-feedback-danger-foreground",
      "peer-focus-visible:outline-2 peer-focus-visible:outline-offset-2 peer-focus-visible:outline-focus",
      "peer-disabled:cursor-not-allowed peer-disabled:opacity-50",
    ].join(" "),
    output: "text-sm tabular-nums text-ink-muted",
  },
});

export type RatingVariants = VariantProps<typeof rating>;
