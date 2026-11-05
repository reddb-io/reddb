// DateRangeField composes Fieldset and DateField; this seam only owns the
// relationship between the two fields and the group's supporting text.

import { tv, type VariantProps } from "tailwind-variants";
import { fieldset } from "./fieldset.variants";

export const dateRangeField = tv({
  slots: {
    root: fieldset().root(),
    // The pair answers to the width it is given, not the window (ADR 0021):
    // side by side from the DS's `md` container step (28rem), stacked below.
    // The size container is a wrapper of its own, so the native fieldset —
    // the component's root — keeps its rendering and its intrinsic width.
    frame: "@container/range-field min-w-0",
    fields: "grid gap-[var(--reddb-spatial-layout-gap-sm)] @md/range-field:grid-cols-2",
    help: fieldset().help(),
    error: fieldset().error(),
  },
});

export type DateRangeFieldVariants = VariantProps<typeof dateRangeField>;
