import { tv, type VariantProps } from "tailwind-variants";

export const DEFAULT_WINDOW_MOCKUP_RATIO = 16 / 9;

export const windowMockup = tv({
  slots: {
    root: "overflow-hidden rounded-lg border border-muted bg-background text-foreground shadow-lg",
    chrome:
      "flex min-h-[var(--reddb-spatial-control-height-sm)] items-center gap-[var(--reddb-spatial-gap-sm)] border-b border-muted bg-muted px-[var(--reddb-spatial-inset-sm)]",
    controls: "flex shrink-0 gap-[var(--reddb-spatial-gap-sm)]",
    control: "size-2 rounded-full bg-muted",
    title: "min-w-0 flex-1 truncate text-center text-xs text-ink-muted",
    balance: "w-10 shrink-0",
    viewport: "bg-background",
  },
});

export type WindowMockupVariants = VariantProps<typeof windowMockup>;
