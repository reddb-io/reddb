<!--
  EmptyState — the universal zero-data surface. Its sentence is required, but
  its media, supporting content, and way out remain caller-owned. In
  particular it does not choose which canonical control an application needs.
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { emptyState, type EmptyStateSize } from "./empty-state.variants";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class" | "title"> {
    /** What is empty, in a few words. */
    title: string;
    /** Why it is empty, or what would fill it. */
    description?: string;
    /** A literal command, path, or key that would fill it. */
    hint?: string;
    /** Defaults to `md`. */
    size?: EmptyStateSize;
    /** Draw the dashed outline around the region. Defaults to true. */
    bordered?: boolean;
    /** An icon or illustration, rendered decoratively above the title. */
    media?: Snippet;
    /** Caller-owned controls rendered after the explanation. */
    actions?: Snippet;
    /** Extra classes merged over the root slot. */
    class?: string;
  }

  const {
    title,
    description,
    hint,
    size = "md",
    bordered = true,
    media,
    actions,
    class: className,
    ...rest
  }: Props = $props();

  const slots = $derived(emptyState({ size, bordered }));
</script>

<div {...(rest as Record<string, unknown>)} class={slots.root({ class: className })}>
  {#if media}
    <div class={slots.media()} aria-hidden="true">{@render media()}</div>
  {/if}

  <p class={slots.title()}>{title}</p>
  {#if description}<p class={slots.description()}>{description}</p>{/if}
  {#if hint}<code class={slots.hint()}>{hint}</code>{/if}
  {#if actions}<div class={slots.actions()}>{@render actions()}</div>{/if}
</div>
