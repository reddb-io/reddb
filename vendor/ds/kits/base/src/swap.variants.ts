// Swap adds only the stable one-cell boundary around ToggleButton's appearance.
import { tv, type VariantProps } from "tailwind-variants";

export const swap = tv({
  base: "inline-grid place-items-center",
});

// A face is one grid item even when its content contains an icon and text.
export const swapFace = tv({
  base: "inline-flex items-center justify-center gap-[var(--reddb-spatial-gap-md)]",
});

export type SwapVariants = VariantProps<typeof swap>;
