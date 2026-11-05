// Carousel's token-only appearance seam; Button owns control appearance and
// the component owns rotation and slide semantics.
import { tv, type VariantProps } from "tailwind-variants";

export const carousel = tv({
  slots: {
    root: "relative min-w-0",
    viewport: "overflow-hidden rounded-md",
    slide: "min-w-0",
    controls: [
      "mt-[var(--reddb-spatial-gap-sm)] flex items-center justify-between",
      "gap-[var(--reddb-spatial-gap-sm)]",
    ].join(" "),
    position: "text-sm text-foreground",
    rotation: "ms-auto",
  },
});

export type CarouselVariants = VariantProps<typeof carousel>;
