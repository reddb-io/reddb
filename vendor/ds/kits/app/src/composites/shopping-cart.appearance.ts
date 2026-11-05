import { tv, type VariantProps } from "tailwind-variants";


/**
 * Token-only arrangement around canonical Base form and list contracts. The
 * Form owns the rhythm between the cart's rows (ADR 0024), so the root adds
 * none. Each item puts its controls beside its details while they fit and
 * wraps them below when they do not — a flex-wrap measured against the item's
 * own width, never the window's (ADR 0021).
 */
export const shoppingCart = tv({
  slots: {
    root: "w-full min-w-0",
    heading: "pb-0",
    list: "m-0 list-none p-0",
    item: "flex min-w-0 flex-wrap items-end gap-[var(--reddb-spatial-gap-md)]",
    details: "grid min-w-0 grow basis-[var(--reddb-container-3xs)] gap-[var(--reddb-spatial-gap-md)]",
    media: "min-w-0 overflow-hidden rounded-md bg-muted",
    identity: "min-w-0",
    name: "m-0 font-medium text-foreground",
    description: "m-0 text-ink-muted",
    price: "m-0 text-foreground",
    controls: "flex flex-wrap items-end gap-[var(--reddb-spatial-gap-md)]",
    itemActions: "flex flex-wrap items-center gap-[var(--reddb-spatial-gap-sm)]",
    quantity: "w-24",
    empty: "min-w-0",
    total: "flex items-center justify-between gap-[var(--reddb-spatial-gap-md)] font-medium",
    actions: "flex flex-wrap items-center justify-end gap-[var(--reddb-spatial-gap-md)]",
  },
});

export type ShoppingCartVariants = VariantProps<typeof shoppingCart>;
