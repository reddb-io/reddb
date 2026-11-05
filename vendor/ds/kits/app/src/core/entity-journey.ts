import type { Tone } from "@reddb-io/design-system/base";
/** Caller-observed execution state; selection never advances the journey. */
export type JourneyState =
  | "complete"
  | "current"
  | "upcoming"
  | "blocked"
  | "failed"
  | "skipped";
export interface JourneyStage {
  /** Stable identity shared with an inspector or scene. */
  id: string;
  /** Visible stage name. */
  label: string;
  /** Explicit observed state, never inferred from position or elapsed time. */
  state: JourneyState;
  /** Optional supporting observation. */
  description?: string;
  /** Caller-formatted elapsed duration or timestamp. */
  elapsed?: string;
  /** Localized state label; omitted, uses the English state name. */
  stateLabel?: string;
}
const states = {
  complete: "Completed",
  current: "In progress",
  upcoming: "Upcoming",
  blocked: "Blocked",
  failed: "Failed",
  skipped: "Skipped",
};
export function journeyStages(items: readonly JourneyStage[]) {
  const ids = new Set<string>();
  return items.map((item) => {
    if (!item.id.trim() || ids.has(item.id))
      throw new Error("EntityJourney: stage ids must be nonempty and unique");
    if (!Object.hasOwn(states, item.state))
      throw new Error("EntityJourney: invalid observed state");
    ids.add(item.id);
    const tone: Tone =
      item.state === "failed"
        ? "danger"
        : item.state === "blocked"
          ? "warning"
          : item.state === "current"
            ? "info"
            : item.state === "complete"
              ? "success"
              : "neutral";
    return { ...item, tone, stateLabel: item.stateLabel ?? states[item.state] };
  });
}
