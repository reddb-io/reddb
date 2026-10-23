// DateField's appearance seam composes the canonical Field and Input skins;
// only the segmented calendar arrangement is new.

import { tv, type VariantProps } from "tailwind-variants";
import { typeRoleMerge } from "./type-roles";
import { field } from "./field.variants";
import { input } from "./input.variants";

const segment = [
  input(),
  "w-[var(--reddb-spatial-control-height-md)] px-0 text-center tabular-nums",
].join(" ");

export const dateField = tv({
  slots: {
    root: field().root(),
    segments: "flex items-center gap-[var(--reddb-spatial-gap-sm)]",
    year: [segment, "w-[calc(var(--reddb-spatial-control-height-md)*1.5)]"].join(" "),
    segment,
    separator: "text-foreground",
  },
}, typeRoleMerge);

export type DateFieldVariants = VariantProps<typeof dateField>;
