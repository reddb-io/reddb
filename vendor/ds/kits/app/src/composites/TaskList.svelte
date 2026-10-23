<script lang="ts">
  import TaskCard from "./TaskCard.svelte";
  import type { TaskListContract } from "../core/contracts/tasklist";
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { EmptyState } from "@reddb-io/design-system/base";
  import { validateTasks, type TaskListItem } from "./task-list.behavior";
  import { taskList } from "./task-list.variants";

  interface Props
    extends
      TaskListContract,
      Omit<
        HTMLAttributes<HTMLDivElement>,
        keyof TaskListContract | "class" | "children"
      > {
    /** Caller-owned controls per task; receives the current item and never assumes an execution API. */
    actions?: Snippet<[TaskListItem]>;
    /** Optional custom empty content, replacing the canonical empty state. */
    empty?: Snippet;
    /** Extra classes on the root. */
    class?: string;
  }
  let {
    label,
    items,
    actions,
    empty,
    emptyLabel = "No tasks",
    emptyDescription,
    class: className,
    ...rest
  }: Props = $props();
  const tasks = $derived(validateTasks(items));
  const slots = taskList();
</script>

<div {...rest} data-task-list class={slots.root({ class: className })}>
  {#if tasks.length}
    <ul aria-label={label} class={slots.list()}>
      {#each tasks as item (item.id)}
        <li data-task-id={item.id} data-state={item.state} class={slots.item()}>
          <TaskCard {item} surface={false} {actions} />
        </li>
      {/each}
    </ul>
  {:else if empty}
    {@render empty()}
  {:else}
    <EmptyState title={emptyLabel} description={emptyDescription} size="sm" />
  {/if}
</div>
