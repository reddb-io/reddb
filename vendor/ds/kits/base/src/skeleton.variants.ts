import { tv, type VariantProps } from "tailwind-variants";

const SHAPE = {
  text: "h-4 w-full rounded-md",
  rectangle: "min-h-[var(--reddb-spatial-control-height-lg)] w-full rounded-md",
  circle: "aspect-square size-12 rounded-full",
} as const;

export const skeleton = tv({
  base: "bg-muted motion-safe:animate-pulse motion-reduce:opacity-75",
  variants: { shape: SHAPE },
  defaultVariants: { shape: "text" },
});

export type SkeletonVariants = VariantProps<typeof skeleton>;
export type SkeletonShape = NonNullable<SkeletonVariants["shape"]>;
export const SKELETON_SHAPES = Object.keys(SHAPE) as readonly SkeletonShape[];
