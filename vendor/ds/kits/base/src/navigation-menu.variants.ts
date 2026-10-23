import { tv, type VariantProps } from "tailwind-variants";

export const navigationMenu = tv({
  slots: {
    root: "relative flex max-w-max items-center",
    list: "flex list-none items-center gap-[var(--reddb-spatial-gap-sm)]",
    control: [
      "inline-flex items-center rounded-md text-foreground",
      // Rest, hover, pressed and focus: the quiet-control contract (wave 6A),
      // through the ghost Button for a trigger and quietControl() for a link.
      "data-[state=open]:bg-foreground/10",
      // The current destination wears the shared selection language: neutral
      // surface, full-weight ink, and a 2px ink edge — on the bottom of a
      // horizontal menu, the start of a vertical one (see `orientation`).
      "border-transparent aria-[current=page]:border-foreground aria-[current=page]:bg-foreground/10",
      "aria-[current=page]:font-medium aria-[current=page]:text-foreground",
    ].join(" "),
    content: "min-w-64",
    links: "grid list-none gap-[var(--reddb-spatial-gap-sm)]",
    link: "block rounded-md p-[var(--reddb-spatial-inset-sm)] text-foreground",
    description: "mt-1 block text-sm text-ink-muted",
  },
  variants: {
    orientation: {
      horizontal: { list: "flex-row", control: "rounded-b-none border-b-2" },
      vertical: { list: "flex-col items-stretch", control: "rounded-s-none border-s-2" },
    },
    size: {
      sm: { control: "h-[var(--reddb-spatial-control-height-sm)] px-[var(--reddb-spatial-inset-sm)] text-sm" },
      md: { control: "h-[var(--reddb-spatial-control-height-md)] px-[var(--reddb-spatial-inset-md)] text-sm" },
      lg: { control: "h-[var(--reddb-spatial-control-height-lg)] px-[var(--reddb-spatial-inset-lg)] text-base" },
    },
  },
  defaultVariants: { orientation: "horizontal", size: "md" },
});

export type NavigationMenuVariants = VariantProps<typeof navigationMenu>;
export type NavigationMenuOrientation = NonNullable<NavigationMenuVariants["orientation"]>;
export type NavigationMenuSize = NonNullable<NavigationMenuVariants["size"]>;
