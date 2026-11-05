import type { TaskListItem } from "../../composites/task-list.behavior";

/** Portable TaskList properties shared by native adapters. */
export interface TaskListContract {
/** Accessible name of the task list. */
    label: string;
/** Caller-owned tasks, in display order, with unique stable ids. */
    items: readonly TaskListItem[];
/** Title of the default empty state. Defaults to No tasks. */
    emptyLabel?: string;
/** Explanation for the default empty state. */
    emptyDescription?: string;
}
