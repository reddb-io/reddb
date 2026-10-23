// Deck: slides stacked in reading order (ADR 0027). The space between slides
// is page layout, so it follows Density from the layout tier (ADR 0024); the
// rhythm inside each slide is the SlideFrame's own.
import { tv, type VariantProps } from "tailwind-variants";

export const deck = tv({
  base: "flex w-full min-w-0 flex-col gap-[var(--reddb-spatial-layout-gap-lg)]",
});

export type DeckVariants = VariantProps<typeof deck>;
