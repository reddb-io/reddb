import { tv, type VariantProps } from "tailwind-variants";

export const DEFAULT_BROWSER_MOCKUP_RATIO = 16 / 9;

export const browserMockup = tv({
  slots: {
    root: "overflow-hidden rounded-lg border border-muted bg-background text-foreground shadow-lg",
    chrome:
      "flex min-h-[var(--reddb-spatial-control-height-sm)] items-center gap-[var(--reddb-spatial-gap-sm)] border-b border-muted bg-muted px-[var(--reddb-spatial-inset-sm)]",
    controls: "flex shrink-0 gap-[var(--reddb-spatial-gap-sm)]",
    control: "size-2 rounded-full bg-muted",
    address:
      "min-w-0 flex-1 truncate rounded-md border border-muted bg-background px-[var(--reddb-spatial-inset-sm)] text-xs text-ink-muted",
    viewport: "bg-background",
  },
});

export type BrowserMockupVariants = VariantProps<typeof browserMockup>;
