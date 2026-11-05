<script module lang="ts">
  export type { StatusCount } from "./core/status-breakdown";
</script>
<script lang="ts">
  import { quietControl } from "@reddb-io/design-system/base";
  import type { StatusBreakdownContract } from "./core/contracts/statusbreakdown";
  import { statusCounts } from "./core/status-breakdown";
  import { statusBreakdown } from "./status-breakdown.variants";
  interface Props extends StatusBreakdownContract {
    /** Additional root classes. */
    class?: string;
  }
  let { label, items, selected = null, interactive = false, onselectedchange, emptyLabel = "No items", class: className }: Props = $props();
  const s = statusBreakdown();
  const resolved = $derived(statusCounts(items));
</script>
<section data-status-breakdown aria-label={label} class={s.root({ class: className })}>
  <p class={s.heading()}>{label}<span class={s.total()}>{resolved.total} total</span></p>
  <div aria-hidden="true" class={s.track()}>
    {#each resolved.items.filter(item => item.count > 0) as item (item.id)}
      <span data-status-segment={item.id} class={s.segment()} style:width={`${item.fraction * 100}%`} style:background={item.paint}></span>
    {/each}
  </div>
  <ul aria-label={`${label} states`} class={s.list()}>
    {#each resolved.items as item (item.id)}
      <li>
        {#snippet content()}<span aria-hidden="true" class={s.swatch()} style:background={item.paint}></span><span>{item.label}</span><span class={s.count()}>{item.count}</span>{/snippet}
        {#if interactive}
          <button type="button" data-status-filter={item.id} aria-pressed={selected === item.id} class={quietControl({ ink: "foreground", class: s.item() })} onclick={() => onselectedchange?.(selected === item.id ? null : item.id)}>{@render content()}</button>
        {:else}<span class={s.item()}>{@render content()}</span>{/if}
      </li>
    {/each}
  </ul>
  {#if resolved.total === 0}<p class={s.empty()}>{emptyLabel}</p>{/if}
</section>
