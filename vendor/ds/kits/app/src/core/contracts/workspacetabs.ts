import type { WorkspaceTab } from "../workspace-tabs";
/** Caller-owned document tabs; no persistence or close decision is inferred. */
export interface WorkspaceTabsContract {
  /** Accessible name for the workspace tablist. */
  label: string;
  /** Documents in display order, with stable unique ids. */
  items: readonly WorkspaceTab[];
  /** Selected document; absent or removed selections fall back to the first enabled item. */
  selected?: string | null;
  /** Requests selection without changing document contents. */
  onselectedchange?: (id: string) => void;
  /** Requests closing; the application confirms dirty drafts and removes accepted items. */
  oncloserequest?: (id: string) => void;
  /** Accessible prefix for each close action. */
  closeLabel?: string;
  /** Accessible description for unsaved changes. */
  dirtyLabel?: string;
  /** Accessible description for active execution. */
  busyLabel?: string;
  /** Explanation when no documents are open. */
  emptyLabel?: string;
}
