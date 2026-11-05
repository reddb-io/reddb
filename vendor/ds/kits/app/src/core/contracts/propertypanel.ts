import type { PropertyItem } from "../operational";
export interface PropertyPanelContract {
  /** Visible and accessible panel name. */
  title: string;
  /** Read-only properties, already formatted by the caller. */
  items: readonly PropertyItem[];
  /** Additional context below the title. */
  description?: string;
}
