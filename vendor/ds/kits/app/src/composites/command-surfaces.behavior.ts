import type { DropdownMenuEntry } from "@reddb-io/design-system/base";

/** One top-level menu in an application Menubar. */
export interface MenubarMenu {
  id: string;
  label: string;
  items: readonly DropdownMenuEntry[];
  disabled?: boolean;
}

/** Context menus share the canonical DropdownMenu row contract. */
export type ContextMenuEntry = DropdownMenuEntry;

/** One action or destination in a Toolbar. */
export interface ToolbarItem {
  id: string;
  label: string;
  href?: string;
  disabled?: boolean;
  onselect?: () => void;
}

/** One action or destination revealed by a SpeedDial. */
export type SpeedDialAction = ToolbarItem;
