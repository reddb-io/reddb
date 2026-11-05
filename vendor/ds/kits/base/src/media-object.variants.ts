// MediaObject owns only the stable media/content relationship; both regions
// remain caller-owned.
import { tv, type VariantProps } from "tailwind-variants";

const ALIGN = {
  start: { root: "items-start" },
  center: { root: "items-center" },
  end: { root: "items-end" },
} as const;

const GAP = {
  sm: { root: "gap-[var(--reddb-spatial-gap-sm)]" },
  md: { root: "gap-[var(--reddb-spatial-gap-md)]" },
  lg: { root: "gap-[var(--reddb-spatial-gap-lg)]" },
} as const;

export const mediaObject = tv({
  slots: {
    root: "flex min-w-0",
    media: "shrink-0",
    content: "min-w-0 flex-1",
  },
  variants: { align: ALIGN, gap: GAP },
  defaultVariants: { align: "start", gap: "md" },
});

export type MediaObjectVariants = VariantProps<typeof mediaObject>;
export type MediaObjectAlign = NonNullable<MediaObjectVariants["align"]>;
export type MediaObjectGap = NonNullable<MediaObjectVariants["gap"]>;
