import { tv, type VariantProps } from "tailwind-variants";
import { dialog } from "./dialog.variants";

/** Dialog appearance narrowed for a focused destructive decision. */
export const alertDialog = tv({
  base: [dialog(), "max-w-md"].join(" "),
});

/** Token-backed arrangement for the canonical decision controls. */
export const alertDialogActions = tv({
  base: [
    "mt-[var(--reddb-spatial-gap-lg)]",
    "flex flex-wrap justify-end gap-[var(--reddb-spatial-gap-md)]",
  ].join(" "),
});

export type AlertDialogVariants = VariantProps<typeof alertDialog>;
