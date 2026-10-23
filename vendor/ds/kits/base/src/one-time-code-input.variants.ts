// OneTimeCodeInput's public appearance seam composes canonical Fieldset and
// Input skins; segment size and group flow are the only added arrangement.

import { tv, type VariantProps } from "tailwind-variants";
import { typeRoleMerge } from "./type-roles";
import { fieldset } from "./fieldset.variants";
import { input } from "./input.variants";

export const oneTimeCodeInput = tv({
  slots: {
    root: fieldset().root(),
    list: "flex flex-wrap gap-[var(--reddb-spatial-gap-sm)]",
    segment: [
      input(),
      // A centered single character needs the whole square, without text-field insets.
      "w-[var(--reddb-spatial-control-height-md)] px-0 text-center",
    ].join(" "),
    help: fieldset().help(),
    error: fieldset().error(),
  },
}, typeRoleMerge);

export type OneTimeCodeInputVariants = VariantProps<typeof oneTimeCodeInput>;
