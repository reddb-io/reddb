import { tv, type VariantProps } from "tailwind-variants";

const GAP = {
  sm: "gap-[var(--reddb-spatial-gap-sm)]",
  md: "gap-[var(--reddb-spatial-gap-md)]",
  lg: "gap-[var(--reddb-spatial-gap-lg)]",
} as const;

export const list = tv({
  base: "flex flex-col",
  variants: { gap: GAP },
  defaultVariants: { gap: "md" },
});

export type ListVariants = VariantProps<typeof list>;
export type ListGap = NonNullable<ListVariants["gap"]>;
export const LIST_GAPS = Object.keys(GAP) as readonly ListGap[];
