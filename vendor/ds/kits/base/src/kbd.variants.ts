import { tv, type VariantProps } from "tailwind-variants";

const SIZE = {
  sm: "h-4 min-w-4 px-1 text-xs",
  md: "h-5 min-w-5 px-1.5 text-xs",
} as const;

/** Key-cap size follows its surrounding typography, not page Density. */
export const kbd = tv({
  base: "inline-flex items-center justify-center rounded-sm border border-muted bg-transparent font-mono leading-tight text-foreground whitespace-nowrap",
  variants: { size: SIZE },
  defaultVariants: { size: "md" },
});

/** A multi-key chord; only the inter-cap gap routes through Density. */
export const kbdChord = tv({
  slots: {
    root: "inline-flex items-center gap-[var(--reddb-spatial-gap-sm)] whitespace-nowrap",
    separator: "text-ink-muted text-xs",
  },
});

export type KbdVariants = VariantProps<typeof kbd>;
export type KbdSize = NonNullable<KbdVariants["size"]>;
export const KBD_SIZES = Object.keys(SIZE) as readonly KbdSize[];
