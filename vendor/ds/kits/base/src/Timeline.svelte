<script module lang="ts">
  export type { TimelineItem } from "./core/timeline";
</script>

<script lang="ts">
  import type { TimelineItem } from "./core/timeline";
  import type { HTMLAttributes } from "svelte/elements";
  import Link from "./Link.svelte";
  import { timeline } from "./timeline.variants";

  interface Props extends Omit<HTMLAttributes<HTMLOListElement>, "class"> {
    /**
     * The entries in display order; each has a `title`, a machine-readable `datetime`, visible
     * `time` text, and optionally a `description` and an `href` that makes the title a link.
     */
    items: readonly TimelineItem[];
    /** Accessible name of the list; defaults to `Timeline`. */
    label?: string;
    /** Extra classes, merged over the root slot's own. */
    class?: string;
  }

  let { items, label = "Timeline", class: className, ...rest }: Props = $props();
  const slots = $derived(timeline());
</script>

<ol
  {...(rest as Record<string, unknown>)}
  data-timeline
  aria-label={label}
  class={slots.root({ class: className })}
>
  {#each items as item (item.id)}
    <li data-timeline-item class={slots.item()}>
      <span aria-hidden="true" class={slots.marker()}></span>
      <span class={slots.content()}>
        <time datetime={item.datetime} class={slots.time()}>{item.time}</time>
        {#if item.href !== undefined}
          <Link href={item.href} class={slots.title()}>{item.title}</Link>
        {:else}
          <span class={slots.title()}>{item.title}</span>
        {/if}
        {#if item.description !== undefined}
          <p class={slots.description()}>{item.description}</p>
        {/if}
      </span>
    </li>
  {/each}
</ol>
