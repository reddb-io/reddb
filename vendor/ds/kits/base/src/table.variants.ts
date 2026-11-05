// Table's token-only appearance seam; native table elements own semantics.
import { tv, type VariantProps } from "tailwind-variants";

export const table = tv({
  slots: {
    root: [
      // A wide native table must not enlarge an ancestor grid/flex item's
      // automatic minimum width. Isolate its inline intrinsic size while
      // retaining native block sizing, sticky headers and scroll behavior.
      "min-w-0 max-w-full overflow-x-auto rounded-md [contain:inline-size]",
      "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus",
    ].join(" "),
    table: "w-full min-w-max text-left text-sm text-foreground",
    caption: [
      "caption-top px-[var(--reddb-spatial-inset-md)]",
      "pb-[var(--reddb-spatial-inset-sm)] text-left font-medium text-foreground",
    ].join(" "),
    header: "sticky top-0 z-10 border-b border-elevation-sunken-border bg-elevation-sunken-surface shadow-elevation-sunken",
    row: "border-b border-muted",
    head: [
      "px-[var(--reddb-spatial-inset-md)] py-[var(--reddb-spatial-inset-sm)]",
      "align-middle font-medium text-foreground",
    ].join(" "),
    cell: [
      "px-[var(--reddb-spatial-inset-md)] py-[var(--reddb-spatial-inset-sm)]",
      "align-middle text-foreground",
    ].join(" "),
  },
});

export type TableVariants = VariantProps<typeof table>;
