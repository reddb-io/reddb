<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import List from "./List.svelte";
  import { gridList, type GridListColumn } from "./grid-list.variants";
  import { list as listAppearance, type ListGap } from "./list.variants";

  interface Props extends Omit<HTMLAttributes<HTMLUListElement>, "class"> {
    /**
     * Most columns the list may use, `1` to `4` (default `1`); it uses fewer as its own width
     * narrows.
     */
    columns?: GridListColumn;
    /** Density-owned space between items: `sm`, `md` (the default) or `lg`. */
    gap?: ListGap;
    /**
     * Whether the grid is a list (default `true`): a `<ul>` whose children are `li` elements. Set
     * `false` for a plain layout grid of blocks that are not list items (KPI tiles, Cards): it
     * renders a `div` with the same columns and gap, and no list semantics (wave 5A).
     */
    list?: boolean;
    /** Extra classes merged onto the list element. */
    class?: string;
    /** The items: `li` elements while `list` is true (a `<ul>` admits no other child), any blocks when it is `false`. */
    children?: Snippet;
  }

  let {
    columns = 1,
    gap = "md",
    list = true,
    class: className,
    children,
    ...rest
  }: Props = $props();
</script>

{#if list}
  <List {...rest} data-grid-list {gap} class={gridList({ columns, class: className })}>
    {@render children?.()}
  </List>
{:else}
  <!-- The same grid without list semantics: a <ul> admits only <li> children. -->
  <div
    {...(rest as Record<string, unknown>)}
    data-grid-list
    class={listAppearance({ gap, class: gridList({ columns, class: className }) })}
  >
    {@render children?.()}
  </div>
{/if}
