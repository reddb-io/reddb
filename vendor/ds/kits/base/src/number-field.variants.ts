import { tv, type VariantProps } from "tailwind-variants";

export const numberField = tv({
  slots: {
    root: "grid min-w-0 grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-[var(--reddb-spatial-gap-sm)]",
    input: "min-w-0 text-center tabular-nums [appearance:textfield] [&::-webkit-inner-spin-button]:appearance-none [&::-webkit-outer-spin-button]:appearance-none",
    step: "aspect-square shrink-0 px-0",
  },
});
export type NumberFieldVariants = VariantProps<typeof numberField>;
