import { tv, type VariantProps } from "tailwind-variants";

export const pagination = tv({
  slots: {
    root: "text-foreground",
    list: "m-0 flex list-none flex-wrap items-center gap-[var(--reddb-spatial-gap-sm)] ps-0",
    page: [
      "inline-flex h-[var(--reddb-spatial-control-height-sm)] min-w-[var(--reddb-spatial-control-height-sm)]",
      "items-center justify-center rounded-md border border-control-edge px-[var(--reddb-spatial-inset-sm)]",
      // The current page wears the shared selection language: a neutral
      // surface, full-weight ink, and a 2px ink underline — not a red
      // outline (DESIGN.md, "Interactive states").
      "aria-[current=page]:border-b-2 aria-[current=page]:border-b-foreground aria-[current=page]:bg-foreground/10",
      "aria-[current=page]:font-medium aria-[current=page]:text-foreground",
    ].join(" "),
  },
});

export type PaginationVariants = VariantProps<typeof pagination>;
