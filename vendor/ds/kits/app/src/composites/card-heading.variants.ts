import { tv, type VariantProps } from "tailwind-variants";

export const cardHeading = tv({
  base: "min-w-0 items-start pb-0",
});

export type CardHeadingVariants = VariantProps<typeof cardHeading>;
