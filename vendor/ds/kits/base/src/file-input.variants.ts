// FileInput's public appearance seam composes the canonical Field and Input
// skins, adding only the visible filename feedback owned by this capability.

import { tv, type VariantProps } from "tailwind-variants";
import { typeRoleMerge } from "./type-roles";
import { field } from "./field.variants";
import { input } from "./input.variants";

export const fileInput = tv({
  slots: {
    root: "grid gap-[var(--reddb-spatial-gap-sm)]",
    field: field().root(),
    control: input(),
    filename: "text-sm text-ink-muted",
  },
}, typeRoleMerge);

export type FileInputVariants = VariantProps<typeof fileInput>;
