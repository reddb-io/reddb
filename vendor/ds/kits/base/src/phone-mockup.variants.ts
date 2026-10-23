import { tv, type VariantProps } from "tailwind-variants";

export const DEFAULT_PHONE_MOCKUP_RATIO = 9 / 16;

export const phoneMockup = tv({
  slots: {
    root:
      "mx-auto w-full max-w-sm rounded-2xl border border-muted bg-background p-[var(--reddb-spatial-inset-sm)] text-foreground shadow-lg",
    chrome:
      "flex h-[var(--reddb-spatial-control-height-sm)] items-center justify-center",
    speaker: "h-1 w-12 rounded-full bg-muted",
    viewport: "rounded-2xl border border-muted bg-background",
  },
});

export type PhoneMockupVariants = VariantProps<typeof phoneMockup>;
