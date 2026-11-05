import { mediaObject } from "@reddb-io/design-system/base/recipes";
import { tv, type VariantProps } from "tailwind-variants";

// Container-first (ADR 0021): the feature is a named size container, and its
// media and content share one line from the DS's `3xl` container step (48rem)
// and stack below it — measured against the feature's own width, never the
// window's. The root cannot query its own width, so the steps are on the two
// regions (a flex-wrap row whose regions are a full line each until they fit
// side by side). `mediaSide="end"` moves the media after the content with
// `order`, so it trails both side by side and stacked. The gap between the
// two is the Density layout tier (ADR 0024).
const ROOT = mediaObject({ gap: "lg" }).root({
  class: "@container/product-feature w-full flex-wrap gap-[var(--reddb-spatial-layout-gap-md)]",
});
const REGION = "min-w-0 grow basis-full @3xl/product-feature:basis-0";

export const productFeature = tv({
  slots: {
    root: ROOT,
    media: `${REGION} overflow-hidden rounded-lg bg-muted`,
    content: REGION,
    heading: "items-start pb-0",
    body: "flex flex-col gap-[var(--reddb-spatial-gap-md)] text-foreground",
    actions: "flex flex-wrap items-center gap-[var(--reddb-spatial-gap-md)]",
  },
  variants: {
    mediaSide: {
      start: {},
      end: { media: "order-last" },
    },
  },
  defaultVariants: { mediaSide: "start" },
});

export type ProductFeatureVariants = VariantProps<typeof productFeature>;
export type ProductFeatureMediaSide = NonNullable<ProductFeatureVariants["mediaSide"]>;
export const PRODUCT_FEATURE_MEDIA_SIDES = ["start", "end"] as const;
