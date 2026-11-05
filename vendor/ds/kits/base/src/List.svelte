<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { list, type ListGap } from "./list.variants";

  interface Props extends Omit<HTMLAttributes<HTMLUListElement>, "class"> {
    /** Render an ordered list instead of the default unordered list. */
    ordered?: boolean;
    /** Density-owned space between items. */
    gap?: ListGap;
    /** Extra classes merged onto the semantic collection. */
    class?: string;
    /** The list items, usually `li` elements. */
    children?: Snippet;
  }

  let { ordered = false, gap = "md", class: className, children, ...rest }: Props = $props();
</script>

{#if ordered}
  <ol {...(rest as Record<string, unknown>)} data-list class={list({ gap, class: className })}>
    {@render children?.()}
  </ol>
{:else}
  <ul {...(rest as Record<string, unknown>)} data-list class={list({ gap, class: className })}>
    {@render children?.()}
  </ul>
{/if}
