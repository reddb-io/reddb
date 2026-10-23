import { tv, type VariantProps } from "tailwind-variants";

/** Token-only modal surface appearance; behavior remains in Dialog.svelte. */
export const dialog = tv({
  base: [
    "m-auto w-full rounded-lg border border-elevation-overlay-border bg-elevation-overlay-surface text-foreground shadow-elevation-overlay",
    "p-[var(--reddb-spatial-inset-lg)]",
    "focus:outline-hidden",
    "backdrop:bg-scrim/60",
    "motion-safe:transition-opacity motion-reduce:transition-none",
  ].join(" "),
  variants: {
    size: {
      sm: "max-w-sm",
      md: "max-w-lg",
      lg: "max-w-2xl",
      xl: "max-w-[var(--reddb-container-wide)]",
    },
  },
  defaultVariants: { size: "md" },
});

/** The standard close control, pinned to the surface's top trailing corner. */
export const dialogClose = tv({
  base: "absolute top-[var(--reddb-spatial-inset-sm)] right-[var(--reddb-spatial-inset-sm)]",
});

export type DialogVariants = VariantProps<typeof dialog>;
export type DialogSize = NonNullable<DialogVariants["size"]>;
export const DIALOG_SIZES = ["sm", "md", "lg", "xl"] as const satisfies readonly DialogSize[];
