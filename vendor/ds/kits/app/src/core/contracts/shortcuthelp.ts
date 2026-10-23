import type { ShortcutGroup } from "../shortcut-help";
/** Contextual keyboard reference; command registration remains application-owned. */
export interface ShortcutHelpContract {
  /** Accessible and visible dialog title. */
  title: string;
  /** Contextual groups with caller-supplied platform-specific key labels. */
  groups: readonly ShortcutGroup[];
  /** Optional invoking control; omit to drive open from the application. */
  triggerLabel?: string;
  /** Whether the keyboard reference is open. */
  open?: boolean;
  /** Reports any requested open-state change, including Escape and dismissal. */
  onopenchange?: (open: boolean) => void;
  /** Empty reference explanation. */
  emptyLabel?: string;
}
