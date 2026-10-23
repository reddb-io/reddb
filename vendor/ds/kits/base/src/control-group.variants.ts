// ControlGroup's public appearance seam. Its gap resolves through the local
// Density scope; orientation changes arrangement, never an appearance axis.

import { tv, type VariantProps } from "tailwind-variants";

export const CONTROL_GROUP_ORIENTATIONS = ["horizontal", "vertical"] as const;
export type ControlGroupOrientation = (typeof CONTROL_GROUP_ORIENTATIONS)[number];

export const controlGroup = tv({
  base: "flex gap-[var(--reddb-spatial-gap-sm)]",
  variants: {
    orientation: {
      horizontal: "flex-row flex-wrap items-center",
      vertical: "flex-col items-stretch",
    },
  },
  defaultVariants: {
    orientation: "horizontal",
  },
});

export type ControlGroupVariants = VariantProps<typeof controlGroup>;
