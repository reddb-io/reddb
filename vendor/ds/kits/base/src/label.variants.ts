// Label's token-only appearance seam. Association stays native through `for`.
import { tv, type VariantProps } from "tailwind-variants";

export const label = tv({
  base: "text-sm font-medium text-foreground",
});

export type LabelVariants = VariantProps<typeof label>;
