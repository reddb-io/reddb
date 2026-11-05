import { tv, type VariantProps } from "tailwind-variants";

export const DIVIDER_ORIENTATIONS = ["horizontal", "vertical"] as const;
export type DividerOrientation = (typeof DIVIDER_ORIENTATIONS)[number];

export const divider = tv({
  base: "shrink-0 bg-muted",
  variants: {
    orientation: {
      horizontal: "my-[var(--reddb-spatial-gap-sm)] h-px w-full",
      vertical: "mx-[var(--reddb-spatial-gap-sm)] h-full w-px self-stretch",
    },
  },
  defaultVariants: { orientation: "horizontal" },
});

export type DividerVariants = VariantProps<typeof divider>;
