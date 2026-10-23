import type { Tone } from "@reddb-io/design-system/base";
export interface ScheduleMarker {
  /** Stable marker identity. */
  id: string;
  /** Visible and accessible marker label, such as Deadline or Now. */
  label: string;
  /** Position in the same caller-owned time unit as the intervals. */
  time: number;
  /** Semantic emphasis; no clock or deadline inference is performed. */
  tone?: Tone;
}
export interface ScheduleWindow {
  /** Stable window identity. */
  id: string;
  /** Visible reason for the unavailable period. */
  label: string;
  /** Inclusive beginning in the intervals' time unit. */
  start: number;
  /** End in the intervals' time unit; must follow start. */
  end: number;
  /** Optional interval row; omitted, shades the entire plot. */
  intervalId?: string;
}
export interface ScheduleOverlays {
  /** Caller-owned deadline, now or reference markers; no timer runs in the component. */
  markers?: readonly ScheduleMarker[];
  /** Caller-owned unavailability windows; the component never reschedules intervals. */
  windows?: readonly ScheduleWindow[];
  /** Shows the built-in selected-interval details; false supports an external linked inspector. */
  showDetails?: boolean;
}
export function scheduleOverlays(
  markers: readonly ScheduleMarker[],
  windows: readonly ScheduleWindow[],
  intervalIds: readonly string[],
) {
  for (const collection of [markers, windows]) {
    const ids = new Set<string>();
    for (const item of collection) {
      if (!item.id || ids.has(item.id))
        throw new Error("Schedule overlays need unique, non-empty ids.");
      ids.add(item.id);
    }
  }
  for (const marker of markers)
    if (!Number.isFinite(marker.time))
      throw new Error("Schedule marker time must be finite.");
  for (const window of windows) {
    if (
      !Number.isFinite(window.start) ||
      !Number.isFinite(window.end) ||
      window.start >= window.end
    )
      throw new Error("Schedule window must have a finite, increasing range.");
    if (
      window.intervalId !== undefined &&
      !intervalIds.includes(window.intervalId)
    )
      throw new Error("Schedule window references an unknown interval.");
  }
  return { markers, windows };
}
export function clippedWindow(
  start: number,
  end: number,
  domain: readonly [number, number],
) {
  const from = Math.max(start, domain[0]),
    to = Math.min(end, domain[1]);
  return to > from ? ([from, to] as const) : null;
}
export function markerPaint(tone: Tone = "neutral") {
  return tone === "neutral"
    ? "var(--reddb-color-foreground)"
    : `var(--reddb-color-feedback-${tone}-foreground)`;
}
