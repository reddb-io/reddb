<script module lang="ts">
  export type { LogEvent } from "./operational";
</script>

<script lang="ts">
  import { untrack } from "svelte";
  import { Button } from "@reddb-io/design-system/base";
  import VirtualList from "./VirtualList.svelte";
  import {
    retainedEvents,
    followEvents,
    operationalToneInk,
  } from "./operational";
  import { operationalView } from "./operational.variants";
  import type { EventLogContract } from "./core/contracts/eventlog";
  interface Props extends EventLogContract {
    /** Additional root classes. */
    class?: string;
  }
  let {
    label,
    events,
    limit = 200,
    query = "",
    following = false,
    onfollowingchange,
    emptyLabel = "No matching events",
    class: className,
  }: Props = $props();
  let host = $state<HTMLDivElement>();
  let suspended = $state(false);
  let unread = $state(0);
  let previous: string | undefined;

  const retained = $derived(retainedEvents(events, limit, query));
  $effect(() => {
    const list = host?.querySelector<HTMLElement>("[data-virtual-list]");
    if (!list) return;
    const controller = followEvents(list, () => {
      suspended = true;
      onfollowingchange?.(false);
    });
    const enabled = following && !untrack(() => suspended);
    controller.update(enabled);
    return () => controller.destroy();
  });
  $effect(() => {
    const last = events.at(-1)?.id,
      follow = following;
    untrack(() => {
      if (follow && !suspended) unread = 0;
      else if (previous && last !== previous) {
        const at = events.findIndex((e) => e.id === previous);
        unread += at < 0 ? events.length : events.length - at - 1;
      }
      previous = last;
    });
    if (host && follow && !suspended) {
      const list = host.querySelector<HTMLElement>("[data-virtual-list]");
      if (list) list.scrollTop = list.scrollHeight;
    }
  });
  const visual = operationalView();
</script>

<div bind:this={host} data-event-log class={visual.root({ class: className })}>
  <div class={visual.header()}>
    <p class={visual.title()}>{label}</p>
    {#if onfollowingchange}<Button
        size="sm"
        variant="ghost"
        onclick={() => {
          const next = !(following && !suspended);
          suspended = false;
          unread = 0;
          onfollowingchange?.(next);
        }}
        >{following && !suspended
          ? "Pause following"
          : `Follow latest${unread ? ` (${unread} new)` : ""}`}</Button
      >{/if}
  </div>
  {#if retained.discarded}<p class={visual.text()}>
      {retained.discarded} earlier events outside retention
    </p>{/if}<VirtualList
    {label}
    items={retained.events}
    rowKey={(event) => event.id}
    rowUnits={2}
    visibleRows={6}
    {emptyLabel}
    >{#snippet children(event)}<div
        data-event-id={event.id}
        class={visual.eventBody()}
      >
        <div class={visual.eventMeta()}>
          <time datetime={event.datetime}>{event.time}</time
          >{#if event.source}<span title={event.source}>{event.source}</span>{/if}
        </div>
        <p
          class={visual.eventMessage()}
          title={event.message}
          style:color={operationalToneInk(event.tone)}
        >
          {event.message}
        </p>
      </div>{/snippet}</VirtualList
  >
</div>
