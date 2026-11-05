import { tv, type VariantProps } from "tailwind-variants";

export const steps = tv({
  slots: {
    root: "text-foreground",
    list: "m-0 grid list-none gap-[var(--reddb-spatial-gap-md)] ps-0",
    item: "min-w-0",
    content: "flex items-center gap-[var(--reddb-spatial-gap-sm)]",
    marker: [
      "inline-flex size-[var(--reddb-spatial-control-height-sm)] shrink-0 items-center justify-center",
      "rounded-full border border-muted text-sm",
    ].join(" "),
    current: "font-medium text-foreground",
    status: "text-sm text-ink-muted",
  },
});

export type StepsVariants = VariantProps<typeof steps>;
