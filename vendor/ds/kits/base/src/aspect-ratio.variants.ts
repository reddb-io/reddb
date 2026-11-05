import { tv, type VariantProps } from "tailwind-variants";

/** The common media ratio; callers remain free to supply any positive ratio. */
export const DEFAULT_ASPECT_RATIO = 16 / 9;

export const aspectRatio = tv({
  base: [
    "relative box-border w-full overflow-hidden",
    "p-[var(--reddb-spatial-inset-sm)]",
    "[&>*]:h-full [&>*]:w-full",
  ].join(" "),
});

export type AspectRatioVariants = VariantProps<typeof aspectRatio>;
