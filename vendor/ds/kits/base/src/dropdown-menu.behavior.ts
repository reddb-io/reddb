import type { IconGlyph } from "./Icon.svelte";
import type * as Portable from "./core/dropdown-menu";
export { dropdownMenuGroups, dropdownMenuItems, isDropdownMenuGroup } from "./core/dropdown-menu";
export type DropdownMenuItem = Portable.DropdownMenuItem<IconGlyph>;
export type DropdownMenuGroup = Portable.DropdownMenuGroup<IconGlyph>;
export type DropdownMenuEntry = Portable.DropdownMenuEntry<IconGlyph>;
