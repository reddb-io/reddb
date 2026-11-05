import type { SidebarRailItem } from "../sidebar-rail";

/** Portable SidebarRail properties shared by native adapters. */
export interface SidebarRailContract {
/** Accessible name for this navigation landmark. */
    label: string;
/** Caller-owned actions, organizations, and optional add affordance. */
    items: readonly SidebarRailItem[];
/** Caller-owned current identity (ADR 0026); SidebarRail never changes it. */
    currentId?: string;
/** Show the destination label below its icon or initials. The rail widens and long labels wrap; defaults to icon-only. */
    showLabels?: boolean;
}
