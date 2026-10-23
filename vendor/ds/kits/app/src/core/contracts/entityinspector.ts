import type { InspectorEntity } from "../entity-inspector";
export interface EntityInspectorContract {
  /** Accessible name of the inspector region. */
  label: string;
  /** Entities in navigation order; filtering and selection remain caller-owned. */
  items: readonly InspectorEntity[];
  /** Selected entity id; null or a missing id shows the empty state. */
  selected: string | null;
  /** Optional visible context path above the title. */
  context?: string;
  /** Requests another selection without changing application state. */
  onselectedchange?: (id: string) => void;
  /** Optional close action; omitted, no close control is rendered. */
  ondismiss?: () => void;
  /** Visible previous-entity action label. */
  previousLabel?: string;
  /** Visible next-entity action label. */
  nextLabel?: string;
  /** Visible close-action label. */
  closeLabel?: string;
  /** Empty-state explanation when no entity is selected. */
  emptyLabel?: string;
  /** Heading level within the surrounding document. */
  headingLevel?: 2 | 3 | 4 | 5 | 6;
}
