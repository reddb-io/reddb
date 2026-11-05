import { tv, type VariantProps } from "tailwind-variants";

export const storeNavigation = tv({
  slots: {
    root: "isolate flex w-full flex-col bg-background text-foreground",
    announcement: [
      "flex min-h-[var(--reddb-spatial-control-height-sm)] items-center justify-center",
      "px-[var(--reddb-spatial-inset-md)] text-center text-sm",
      "bg-primary text-on-primary",
    ].join(" "),
    navbar: "relative",
  },
});

export type StoreNavigationVariants = VariantProps<typeof storeNavigation>;
