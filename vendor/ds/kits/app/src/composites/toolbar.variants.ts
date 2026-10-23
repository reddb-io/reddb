import { tv, type VariantProps } from "tailwind-variants";

export const toolbar = tv({
  slots: {
    root: [
      "inline-flex items-center gap-[var(--reddb-spatial-gap-sm)]",
      "data-[command-toolbar]:items-center",
      "rounded-md border border-muted bg-background p-[var(--reddb-spatial-inset-sm)]",
    ].join(" "),
    item: "inline-flex shrink-0 items-center",
  },
  variants: {
    orientation: {
      horizontal: "",
      vertical: { root: "flex-col items-stretch" },
    },
    size: {
      sm: { item: "h-[var(--reddb-spatial-control-height-sm)] px-[var(--reddb-spatial-inset-sm)] text-sm" },
      md: { item: "h-[var(--reddb-spatial-control-height-md)] px-[var(--reddb-spatial-inset-md)] text-sm" },
      lg: { item: "h-[var(--reddb-spatial-control-height-lg)] px-[var(--reddb-spatial-inset-lg)] text-base" },
    },
  },
  defaultVariants: { orientation: "horizontal", size: "md" },
});

export type ToolbarVariants = VariantProps<typeof toolbar>;
export type ToolbarSize = NonNullable<ToolbarVariants["size"]>;
