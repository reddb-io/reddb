import type { TaskListItem } from "../../composites/task-list.behavior";
export interface TaskCardContract {
  /** Caller-owned task lifecycle and progress, shared with TaskList. */
  item: TaskListItem;
  /** Draw the enclosing surface; false embeds the same task content in a list. */
  surface?: boolean;
}
