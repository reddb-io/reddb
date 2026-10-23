// Container's public appearance seam. Its width is structural and DS-named
// (ADR 0021): `prose` is the reading measure, `content` a single column,
// `wide` the page frame (80rem, the former `max-w-7xl` and the default) and
// `full` no limit. Only the inline inset belongs to Density, so that role
// remains live in every scope.
//
// It declares no size container itself: containment would make it the
// containing block of every fixed-position descendant a page places in it.
// Components that step with width declare their own (ADR 0021).
import { tv, type VariantProps } from "tailwind-variants";

const SIZE = {
  prose: "max-w-prose",
  content: "max-w-content",
  wide: "max-w-wide",
  full: "max-w-none",
} as const;

export const container = tv({
  base: "mx-auto box-border w-full px-[var(--reddb-spatial-inset-md)]",
  variants: { size: SIZE },
  defaultVariants: { size: "wide" },
});

export type ContainerVariants = VariantProps<typeof container>;
export type ContainerSize = NonNullable<ContainerVariants["size"]>;
export const CONTAINER_SIZES = Object.keys(SIZE) as readonly ContainerSize[];
