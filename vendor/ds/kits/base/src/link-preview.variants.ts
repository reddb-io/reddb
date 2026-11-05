import { tv, type VariantProps } from "tailwind-variants";
import { popover } from "./popover.variants";

/** Link previews reuse the canonical anchored surface instead of restating it. */
export const linkPreview = tv({
  extend: popover,
  base: "select-text",
});

export type LinkPreviewVariants = VariantProps<typeof linkPreview>;
