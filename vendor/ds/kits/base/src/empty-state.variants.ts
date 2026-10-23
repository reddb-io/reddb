// EmptyState's appearance is one coordinated slot contract. The size axis
// changes typography and Density-owned space together; bordered only decides
// whether the otherwise empty region needs to draw its own boundary.
import { tv, type VariantProps } from "tailwind-variants";

const SIZE = {
  sm: {
    root: "gap-[var(--reddb-spatial-gap-md)] px-[var(--reddb-spatial-inset-md)] py-[var(--reddb-spatial-inset-lg)]",
    title: "text-sm",
    description: "text-xs",
    hint: "text-xs",
  },
  md: {
    root: "gap-[var(--reddb-spatial-gap-lg)] px-[var(--reddb-spatial-inset-lg)] py-10",
    title: "text-base",
    description: "text-sm",
    hint: "text-xs",
  },
} as const;

const BORDERED = {
  true: { root: "border-dashed border-muted" },
  false: { root: "border-transparent" },
} as const;

export const emptyState = tv({
  slots: {
    root: "flex flex-col items-center justify-center rounded-lg border text-center",
    media: "text-ink-muted",
    title: "font-medium leading-tight text-foreground",
    description: "max-w-prose text-ink-muted",
    hint: "max-w-full rounded-md border border-muted px-2 py-1 font-mono text-ink-muted",
    actions: "flex flex-wrap items-center justify-center gap-[var(--reddb-spatial-gap-md)] pt-1",
  },
  variants: { size: SIZE, bordered: BORDERED },
  defaultVariants: { size: "md", bordered: true },
});

export type EmptyStateVariants = VariantProps<typeof emptyState>;
export type EmptyStateSize = NonNullable<EmptyStateVariants["size"]>;
export const EMPTY_STATE_SIZES = Object.keys(SIZE) as readonly EmptyStateSize[];
