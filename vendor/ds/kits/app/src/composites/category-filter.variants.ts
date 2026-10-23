import { tv, type VariantProps } from "tailwind-variants";

/** One independently selectable category in a CategoryFilter. */
export interface CategoryFilterOption {
  /** Stable native form value. */
  value: string;
  /** Visible category name. */
  label: string;
  /** Present but unavailable to pointer and keyboard selection. */
  disabled?: boolean;
}

/** Token-only arrangement around canonical Base Fieldset and Checkbox contracts. */
export const categoryFilter = tv({
  slots: {
    root: "min-w-0",
    list: "grid w-full gap-[var(--reddb-spatial-gap-sm)]",
    option: "min-w-0",
  },
});

export type CategoryFilterVariants = VariantProps<typeof categoryFilter>;
