// ListRow's styling. See button.variants.ts for the split, the colour rule and
// the spatial rule.
//
// Slots, because a row is a rail: a leading affordance, a two-line text block
// that must truncate rather than wrap, and a trailing rail pushed to the end.
// Those four have to agree about height and gap, so one `size` moves them
// together.
//
// `interactive` is not a caller's choice — the component sets it from whether
// the row actually does anything (see ListRow.svelte). A row that looks
// pressable and is not is the lie this axis exists to prevent, so it cannot be
// set independently of the element that carries the behavior.

import { tv, type VariantProps } from "tailwind-variants";

// The row's `size` picks WHICH spatial role each slot wears, and the
// document's Density stop decides what those roles resolve to. The prop used
// to be called `density`, reusing two Density stop names for a per-component
// choice; ADR 0026 keeps Density words for the axis alone, so the scale is
// `size` (`md`, formerly `comfortable`; `sm`, formerly `compact`). A small row
// inside a spacious page is therefore a small row, laid out spaciously — which
// is the arrangement a dense table inside a marketing page actually wants
// (ADR 0003).
const SIZE = {
  /** The default: a row you can hit with a thumb. */
  md: {
    root: "gap-[var(--reddb-spatial-gap-lg)] px-[var(--reddb-spatial-inset-md)] py-[var(--reddb-spatial-inset-sm)]",
  },
  /** For long lists where the scan matters more than the target. */
  sm: {
    root: "min-h-[var(--reddb-spatial-control-height-md)] gap-[var(--reddb-spatial-gap-md)] px-[var(--reddb-spatial-inset-sm)] py-[var(--reddb-spatial-gap-md)]",
  },
} as const;

/**
 * The `density` values kept for one release (ADR 0026), and the `size` each
 * one now is.
 */
export const DEPRECATED_LIST_ROW_DENSITIES = {
  comfortable: "md",
  compact: "sm",
} as const satisfies Record<string, keyof typeof SIZE>;

const INTERACTIVE = {
  true: {
    // Hover, pressed, focus and disabled are the quiet-control contract the
    // component wraps an interactive row in (wave 6A).
    root: "cursor-pointer",
  },
  false: { root: "" },
} as const;

const SELECTED = {
  /** The row this list is currently about. */
  true: { root: "bg-foreground/10", title: "font-medium text-foreground" },
  false: { root: "" },
} as const;

export const listRow = tv({
  slots: {
    // `text-start` rather than the browser's default, because this same row is
    // sometimes a <button>, which centres its text.
    root: "flex w-full items-center border-b border-muted text-start",
    leading: "flex shrink-0 items-center",
    text: "flex min-w-0 flex-col gap-0.5",
    title: "truncate text-sm font-medium text-foreground",
    description: "truncate text-xs text-ink-muted",
    trailing: "ms-auto flex shrink-0 items-center gap-[var(--reddb-spatial-gap-md)]",
  },
  variants: { size: SIZE, interactive: INTERACTIVE, selected: SELECTED },
  defaultVariants: { size: "md", interactive: false, selected: false },
});

export type ListRowVariants = VariantProps<typeof listRow>;
export type ListRowSize = NonNullable<ListRowVariants["size"]>;

export const LIST_ROW_SIZES = Object.keys(SIZE) as readonly ListRowSize[];

/** @deprecated ListRow's scale is `size` (ADR 0026); use `ListRowSize`. Removed next release. */
export type ListRowDensity = keyof typeof DEPRECATED_LIST_ROW_DENSITIES;
/** @deprecated ListRow's scale is `size` (ADR 0026); use `LIST_ROW_SIZES`. Removed next release. */
export const LIST_ROW_DENSITIES = Object.keys(DEPRECATED_LIST_ROW_DENSITIES) as readonly ListRowDensity[];
