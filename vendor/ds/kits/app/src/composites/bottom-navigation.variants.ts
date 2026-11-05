import { tv, type VariantProps } from "tailwind-variants";

export const bottomNavigation = tv({
  slots: {
    root: "w-full border-t border-muted bg-elevation-sunken-surface pb-[env(safe-area-inset-bottom)] text-foreground",
    face: "inline-flex size-[var(--reddb-spatial-icon-size-md)] items-center justify-center",
    badge: "h-[var(--reddb-spatial-icon-size-md)] min-w-[var(--reddb-spatial-icon-size-md)] justify-center rounded-full p-0 tabular-nums",
    list: [
      "m-0 flex list-none items-stretch gap-[var(--reddb-spatial-gap-sm)] p-0",
      "px-[var(--reddb-spatial-inset-sm)] py-[var(--reddb-spatial-inset-sm)]",
    ].join(" "),
    entry: "min-w-0 flex-1",
    // A horizontal rail: the current item's 2px ink indicator moves from
    // NavItem's start edge to the top edge, the one the bar shares with the
    // content above it.
    item: "min-h-[var(--reddb-spatial-control-height-lg)] justify-center rounded-none border-s-0 border-t-2 px-0 text-center flex-col gap-[var(--reddb-spatial-gap-sm)] text-caption [&_[data-nav-label]]:max-w-full",
  },
});

export type BottomNavigationVariants = VariantProps<typeof bottomNavigation>;
