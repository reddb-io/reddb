// The bypass is visually absent until focus, then becomes a token-painted
// fixed control above ordinary page content. Motion has an explicit off-ramp.
import { tv, type VariantProps } from "tailwind-variants";

export const skipLink = tv({
  base: [
    "sr-only focus:not-sr-only",
    "focus:fixed focus:start-[var(--reddb-spatial-inset-md)] focus:top-[var(--reddb-spatial-inset-md)] focus:z-50",
    "focus:h-auto focus:w-auto focus:overflow-visible focus:whitespace-normal",
    "focus:rounded-md focus:border focus:border-muted focus:bg-background focus:text-foreground",
    "focus:px-[var(--reddb-spatial-inset-md)] focus:py-[var(--reddb-spatial-inset-sm)]",
    "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus",
    "transition-opacity motion-reduce:transition-none",
  ].join(" "),
});

export type SkipLinkVariants = VariantProps<typeof skipLink>;
