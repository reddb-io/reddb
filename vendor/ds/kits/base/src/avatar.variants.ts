// Avatar's token-only appearance seam; image loading and fallback semantics
// remain in the component.
import { tv, type VariantProps } from "tailwind-variants";

const SIZE = {
  sm: {
    root: "size-[var(--reddb-spatial-control-height-sm)] text-xs",
  },
  md: {
    root: "size-[var(--reddb-spatial-control-height-md)] text-sm",
  },
  lg: {
    root: "size-[var(--reddb-spatial-control-height-lg)] text-base",
  },
} as const;

export const avatar = tv({
  slots: {
    root: "relative inline-flex shrink-0 overflow-hidden rounded-full bg-muted text-foreground",
    image: "absolute inset-0 size-full object-cover",
    fallback: "flex size-full items-center justify-center font-medium",
  },
  variants: { size: SIZE },
  defaultVariants: { size: "md" },
});

export type AvatarVariants = VariantProps<typeof avatar>;
export type AvatarSize = NonNullable<AvatarVariants["size"]>;
export const AVATAR_SIZES = Object.keys(SIZE) as readonly AvatarSize[];
