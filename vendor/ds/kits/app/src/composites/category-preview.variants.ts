import { card } from "@reddb-io/design-system/base/recipes";
import { tv, type VariantProps } from "tailwind-variants";

const ROOT = card({ variant: "plain", padding: "none" }).root({
  class: "group h-full min-w-0 overflow-hidden",
});

/** Token-only presentation around canonical Card, AspectRatio, and Link contracts. */
export const categoryPreview = tv({
  slots: {
    root: ROOT,
    media: "overflow-hidden rounded-lg bg-muted",
    content: "flex min-w-0 flex-col gap-[var(--reddb-spatial-gap-sm)] pt-[var(--reddb-spatial-inset-sm)]",
    // The destination's name reads in the Theme's `heading` role, through the
    // Base Heading (ADR 0025).
    name: "",
    description: "text-sm text-ink-muted",
  },
});

export type CategoryPreviewVariants = VariantProps<typeof categoryPreview>;
