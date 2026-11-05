import { dropdownMenu, popover, type DropdownMenuSize } from "@reddb-io/design-system/base/recipes";
import { tv, type VariantProps } from "tailwind-variants";

// Rows, headings, separators, icons and their three sizes are the Base
// DropdownMenu's: one menu vocabulary, extended rather than copied, so the
// three menus cannot drift apart. Only what this surface adds lives here.
export const menubar = tv({
  extend: dropdownMenu,
  slots: {
    root: "flex items-center gap-[var(--reddb-spatial-gap-sm)] data-[command-menubar]:items-center",
    content: popover({ class: "min-w-48 p-[var(--reddb-spatial-inset-sm)]" }),
  },
});

export type MenubarVariants = VariantProps<typeof menubar>;
// The sizes are the DropdownMenu's; `extend` widens the variant's inferred
// type to string, so the size is named from its source instead.
export type MenubarSize = DropdownMenuSize;
