import { tv, type VariantProps } from "tailwind-variants";

/** One application filter offered by the single-active Filter surface. */
export interface FilterOption {
  /** Stable submitted value. */
  value: string;
  /** Visible option name. */
  label: string;
  /** Present but unavailable to pointer and keyboard selection. */
  disabled?: boolean;
}

/** Token-only arrangement around the canonical Base ToggleGroup. */
export const filter = tv({
  slots: {
    root: "min-w-0 w-full",
    list: "gap-[var(--reddb-spatial-gap-sm)]",
  },
});

export type FilterVariants = VariantProps<typeof filter>;
