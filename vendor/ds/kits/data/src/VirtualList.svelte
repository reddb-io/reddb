<script lang="ts" generics="T">
  import type { VirtualListContract } from "./core/contracts/virtuallist";
  import { tick } from "svelte";
  import type { Snippet } from "svelte";
  import { virtualRange, virtualKeys } from "./virtual-list";
  import { virtualList } from "./virtual-list.variants";

  interface Props extends VirtualListContract<T> {
    /** Content for one fixed-height row. Keep its controls within the declared height. */
    children: Snippet<[T, number]>;
    /** Extra classes on the scroll region. */
    class?: string;
  }
  let { label, items, rowKey, rowUnits = 2, visibleRows = 6, overscan = 2, children,
    emptyLabel = "No items.", class: className }: Props = $props();
  const styles = virtualList();
  let root = $state<HTMLDivElement>();
  let unit = $state(0);
  let top = $state(0);
  let focused = $state<string>();
  const keys = $derived(virtualKeys(items,rowKey,rowUnits,visibleRows));
  const height = $derived((unit || 32) * rowUnits);
  const range = $derived(virtualRange(items.length, height, height * visibleRows, top, overscan));
  const indices = $derived.by(() => {
    const result = Array.from({ length: range.end - range.start }, (_, i) => range.start + i);
    const pinned = focused === undefined ? -1 : keys.indexOf(focused);
    if (pinned >= 0 && !result.includes(pinned)) result.push(pinned);
    return result.sort((a,b) => a-b);
  });
  $effect(() => {
    if (root && Math.abs(root.scrollTop - range.top) > 1) root.scrollTop = range.top;
    if (focused !== undefined && !keys.includes(focused)) { focused = undefined; void tick().then(() => root?.focus()); }
  });
  const rowHeight = $derived(`calc(var(--reddb-spatial-control-height-md) * ${rowUnits})`);
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex (The named native scroll region must be reachable for keyboard scrolling.) -->
<div bind:this={root} data-virtual-list class={styles.root({ class: className })} role="region" aria-label={label} tabindex="0"
  style:height={`calc(var(--reddb-spatial-control-height-md) * ${rowUnits * visibleRows})`}
  onscroll={(event) => top = event.currentTarget.scrollTop}
  onfocusin={(event) => { const row = (event.target as HTMLElement).closest<HTMLElement>("[data-virtual-key]"); focused = row?.dataset.virtualKey; }}
  onfocusout={(event) => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) focused = undefined; }}>
  <span aria-hidden="true" class={styles.probe()} bind:clientHeight={unit}></span>
  {#if items.length}
    <ul class={styles.list()} aria-label={label}>
      {#each indices as index (keys[index])}
        {@const previous = indices.indexOf(index) === 0 ? -1 : indices[indices.indexOf(index) - 1]!}
        {#if index > previous + 1}<li role="presentation" aria-hidden="true" style:height={`calc(${rowHeight} * ${index - previous - 1})`}></li>{/if}
        <li class={styles.row()} style:height={rowHeight} aria-posinset={index + 1} aria-setsize={items.length} data-virtual-key={keys[index]}>
          {@render children(items[index]!, index)}
        </li>
      {/each}
      {#if (indices.at(-1) ?? -1) < items.length - 1}<li role="presentation" aria-hidden="true" style:height={`calc(${rowHeight} * ${items.length - (indices.at(-1) ?? -1) - 1})`}></li>{/if}
    </ul>
  {:else}<p class={styles.empty()}>{emptyLabel}</p>{/if}
</div>
