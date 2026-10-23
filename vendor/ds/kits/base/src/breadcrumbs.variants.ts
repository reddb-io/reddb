import { tv, type VariantProps } from "tailwind-variants";
import { typeRoleMerge } from "./type-roles";

export const breadcrumbs = tv({
  slots: {
    root: "text-caption font-regular text-ink-muted",
    list: "m-0 flex list-none flex-wrap items-center gap-[var(--reddb-spatial-gap-lg)] ps-0",
    item: "flex items-center gap-[var(--reddb-spatial-gap-lg)]",
    current: "font-regular text-foreground",
    link: "font-regular text-ink-muted hover:text-foreground focus-visible:text-foreground underline-offset-[calc(var(--reddb-spatial-gap-sm)/2)]",
    separator: "select-none text-ink-muted/70",
  },
}, typeRoleMerge);

export type BreadcrumbsVariants = VariantProps<typeof breadcrumbs>;
