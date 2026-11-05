<script lang="ts">
  import type { Snippet } from "svelte";
  import {
    Link,
    Progress,
    StatusIndicator,
    formatPercent,
    normalizeRange,
  } from "@reddb-io/design-system/base";
  import type { TaskCardContract } from "../core/contracts/taskcard";
  import {
    TASK_STATE_LABELS,
    TASK_STATE_TONES,
    validateTasks,
    type TaskListItem,
  } from "./task-list.behavior";
  import { taskList } from "./task-list.variants";
  import { operational } from "../operational.variants";
  interface Props extends TaskCardContract {
    /** Application-owned controls; the card never schedules work. */
    actions?: Snippet<[TaskListItem]>;
    /** Additional root classes. */
    class?: string;
  }
  let { item, surface = true, actions, class: className }: Props = $props();
  const task = $derived(validateTasks([item])[0]!);
  const slots = taskList(),
    frame = operational();
</script>

<div
  data-task-card
  data-state={task.state}
  class={[surface ? frame.root() : "", frame.body(), className]
    .filter(Boolean)
    .join(" ")}
>
  <div class={slots.header()}>
    <p class={slots.title()}>
      {#if task.href}<Link href={task.href}>{task.title}</Link
        >{:else}{task.title}{/if}
    </p>
    <StatusIndicator
      label={task.stateLabel ?? TASK_STATE_LABELS[task.state]}
      tone={task.tone ?? TASK_STATE_TONES[task.state]}
    />
  </div>
  {#if task.description}<p class={slots.description()}>
      {task.description}
    </p>{/if}
  {#if task.progress !== undefined}<div class={slots.progress()}>
      <Progress
        label={`${task.title} progress`}
        value={task.progress}
        formatValue={task.progressLabel ? () => task.progressLabel! : undefined}
        class="min-w-0 flex-1"
      />{#if task.progress !== null}<span
          aria-hidden="true"
          class={slots.reading()}
          >{task.progressLabel ??
            formatPercent(normalizeRange(task.progress, 0, 100))}</span
        >{/if}
    </div>{/if}
  {#if task.worker || task.estimate}<div class={slots.context()}>
      {#if task.worker}<span>{task.worker}</span>{/if}{#if task.estimate}<span
          >{task.estimate}</span
        >{/if}
    </div>{/if}
  {#if task.reason}<p class={slots.reason()}>{task.reason}</p>{/if}
  {#if actions}<div class={slots.actions()}>{@render actions(task)}</div>{/if}
</div>
