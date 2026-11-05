import type { Tone } from "@reddb-io/design-system/base";

export const TASK_STATES = ["queued", "running", "paused", "blocked", "succeeded", "failed", "cancelled"] as const;
export type TaskState = (typeof TASK_STATES)[number];

export interface TaskListItem {
  /** Stable identity across updates, filtering and ordering. */
  id: string;
  /** Visible task name, used to name its progress. */
  title: string;
  /** Caller-owned lifecycle state; the list never starts or schedules work. */
  state: TaskState;
  /** Localized state name; defaults to the English lifecycle name. */
  stateLabel?: string;
  /** Optional semantic emphasis; otherwise follows the lifecycle state. */
  tone?: Tone;
  /** Supporting task context. */
  description?: string;
  /** Progress from 0 to 100, clamped by Progress; null means indeterminate. Omit to hide the track. */
  progress?: number | null;
  /** Caller-formatted progress reading, if different from a percentage. */
  progressLabel?: string;
  /** Worker or other execution context, already formatted by the caller. */
  worker?: string;
  /** Caller-formatted remaining-time estimate; no timers run in the component. */
  estimate?: string;
  /** Visible reason for a blocked, paused or failed task, or other relevant explanation. */
  reason?: string;
  /** Optional destination for the task name. */
  href?: string;
}

export const TASK_STATE_LABELS: Record<TaskState, string> = {
  queued: "Queued", running: "Running", paused: "Paused", blocked: "Blocked",
  succeeded: "Completed", failed: "Failed", cancelled: "Cancelled",
};
export const TASK_STATE_TONES: Record<TaskState, Tone> = {
  queued: "neutral", running: "info", paused: "neutral", blocked: "warning",
  succeeded: "success", failed: "danger", cancelled: "neutral",
};

export function validateTasks(items: readonly TaskListItem[]): readonly TaskListItem[] {
  const ids = new Set<string>();
  for (const item of items) {
    if (!item.id || ids.has(item.id)) throw new Error("TaskList: task ids must be non-empty and unique.");
    if (!TASK_STATES.includes(item.state)) throw new Error(`TaskList: unknown state for ${item.id}.`);
    if (item.progress !== undefined && item.progress !== null && !Number.isFinite(item.progress)) throw new Error("TaskList: progress must be finite, null or omitted.");
    ids.add(item.id);
  }
  return items;
}
