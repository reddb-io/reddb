import { tv, type VariantProps } from "tailwind-variants";

/** Token-only layout around the canonical Popover and Combobox appearances. */
export const commandPalette = tv({
  slots: {
    root: "isolate min-w-0 text-foreground",
    content: [
      "w-[min(32rem,calc(100vw-2rem))]",
      "p-[var(--reddb-spatial-inset-md)]",
    ].join(" "),
    search: "min-w-0",
  },
});

export type CommandPaletteVariants = VariantProps<typeof commandPalette>;
