// Select's public appearance seam. The component keeps the platform's own
// popup and keyboard model while giving the closed control a canonical skin.

import { tv, type VariantProps } from "tailwind-variants";
import { typeRoleMerge } from "./type-roles";

export const select = tv({
  base: [
    "flex w-full cursor-pointer [&[multiple]]:h-auto",
    "rounded-md border border-control-edge bg-background",
    "text-foreground",
    "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus",
    "aria-invalid:border-feedback-danger-border",
    "disabled:cursor-not-allowed disabled:opacity-50",
  ].join(" "),
  variants: {
    // The same three steps as Button, so a field and the action beside it
    // share one control height at every Density stop. Named `controlSize` on
    // the component: the native \`size\` attribute is still spread through.
    size: {
      sm: "h-[var(--reddb-spatial-control-height-sm)] px-[var(--reddb-spatial-inset-sm)] text-editable-sm",
      md: "h-[var(--reddb-spatial-control-height-md)] px-[var(--reddb-spatial-inset-md)] text-editable-md",
      lg: "h-[var(--reddb-spatial-control-height-lg)] px-[var(--reddb-spatial-inset-lg)] text-editable-lg",
    },
  },
  defaultVariants: { size: "md" },
}, typeRoleMerge);

export type SelectVariants = VariantProps<typeof select>;
export type SelectSize = NonNullable<SelectVariants["size"]>;
