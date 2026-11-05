import type { JourneyStage } from "../entity-journey";
/** Portable execution trail with caller-owned state and selection. */
export interface EntityJourneyContract {
  /** Accessible name for the ordered trail. */
  label: string;
  /** Ordered observed stages; no overall completion is inferred. */
  items: readonly JourneyStage[];
  /** Inspected stage, independent of current execution state. */
  selected?: string | null;
  /** Requests inspection; omitted, the trail is read-only. */
  onselectedchange?: (id: string) => void;
  /** Explanation when no stages are present. */
  emptyLabel?: string;
}
