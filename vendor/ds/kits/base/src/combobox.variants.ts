// Combobox's caller-owned options and token-only appearance seam.

import { tv, type VariantProps } from "tailwind-variants";

export interface ComboboxOption {
  /** Submitted value for the option. */
  value: string;
  /** Visible text and the text matched by typeahead filtering. */
  label: string;
  /** Removes only this option from interaction and selection. */
  disabled?: boolean;
}

export const combobox = tv({
  slots: {
    root: "relative min-w-0",
    control: "relative min-w-0",
    input: "",
    trigger: [
      "absolute inset-e-0 top-0 inline-flex items-center justify-center text-foreground",
      "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus",
      "disabled:cursor-not-allowed disabled:opacity-50",
    ].join(" "),
    content: [
      "max-h-[min(70dvh,var(--bits-combobox-content-available-height))] w-[var(--bits-combobox-anchor-width)] overflow-y-auto",
      "p-[var(--reddb-spatial-inset-sm)]",
    ].join(" "),
    item: [
      "flex cursor-default items-center justify-between rounded-md",
      "min-h-[var(--reddb-spatial-control-height-md)] gap-[var(--reddb-spatial-gap-sm)]",
      "px-[var(--reddb-spatial-inset-md)] text-sm",
      // Highlight and selection are neutral: the Brand red marks the primary
      // action, never the option under the cursor (DESIGN.md, "Interactive states").
      "text-foreground data-[highlighted]:bg-foreground/10 data-[selected]:font-medium",
      "data-[disabled]:cursor-not-allowed data-[disabled]:opacity-50",
    ].join(" "),
    indicator: "font-medium",
    empty: "block px-[var(--reddb-spatial-inset-sm)] py-[var(--reddb-spatial-inset-sm)] text-sm text-ink-muted",
  },
  variants: {
    // The input's clearance and the trigger's square are one control height,
    // so the trigger sits flush whichever size the field is.
    size: {
      sm: {
        input: "pe-[var(--reddb-spatial-control-height-sm)]",
        trigger: "size-[var(--reddb-spatial-control-height-sm)]",
      },
      md: {
        input: "pe-[var(--reddb-spatial-control-height-md)]",
        trigger: "size-[var(--reddb-spatial-control-height-md)]",
      },
      lg: {
        input: "pe-[var(--reddb-spatial-control-height-lg)]",
        trigger: "size-[var(--reddb-spatial-control-height-lg)]",
      },
    },
  },
  defaultVariants: { size: "md" },
});

export type ComboboxVariants = VariantProps<typeof combobox>;
export type ComboboxSize = NonNullable<ComboboxVariants["size"]>;
