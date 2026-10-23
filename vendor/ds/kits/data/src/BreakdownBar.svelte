<script module lang="ts">
  export * from "./core/breakdown-bar";
</script>

<script lang="ts">
  import { type BreakdownItem, breakdownItems } from "./core/breakdown-bar";
  import type { BreakdownBarContract } from "./core/contracts/breakdownbar";
  import { quietControl } from "@reddb-io/design-system/base";
  import { formatChartNumber, type ChartNumberFormat } from "./chart";

  interface Props extends BreakdownBarContract {
    /** Called when a group is selected or cleared, for application-owned drill-down. */
    onselectedchange?: (id: string | null) => void;
    /** Extra classes merged on the composition. */
    class?: string;
  }
  let { label, items, format = formatChartNumber, selected = $bindable(null), onselectedchange, class: className = "" }: Props = $props();
  const resolved = $derived(breakdownItems(items));
  const active = $derived(resolved.some(item => item.id === selected) ? selected : null);
  const total = $derived(resolved.reduce((sum, item) => sum + item.value, 0));
  function choose(id: string | null): void {
    selected = id === active ? null : id;
    onselectedchange?.(selected);
  }
</script>

<div data-breakdown-bar class={`grid min-w-0 gap-[var(--reddb-spatial-gap-sm)] ${className}`}>
  <div role="img" aria-label={`${label}: ${format(total)} total. ${resolved.map(item => `${item.label} ${(item.fraction * 100).toFixed(1)}%`).join(", ")}`}
    data-breakdown-segments class="flex h-[var(--reddb-spatial-control-height-sm)] overflow-hidden rounded-md bg-muted">
    {#each resolved.filter(item => item.value > 0) as item (item.id)}
      <span data-breakdown-segment={item.id} aria-hidden="true" class="min-w-0 h-full border-e border-background last:border-e-0"
        style:width={`${item.fraction * 100}%`} style:background={item.paint} style:opacity={active && active !== item.id ? 0.35 : 1}></span>
    {/each}
  </div>
  <div role="group" aria-label={`${label} groups`} class="flex min-w-0 flex-wrap gap-x-[var(--reddb-spatial-gap-md)] gap-y-[var(--reddb-spatial-gap-sm)]">
    {#each resolved as item (item.id)}
      <button type="button" aria-pressed={active === item.id} data-breakdown-group={item.id}
        class={quietControl({ ink: "foreground", class: "inline-flex min-h-[var(--reddb-spatial-control-height-sm)] items-center gap-[var(--reddb-spatial-gap-sm)] border-b-2 border-transparent text-caption aria-pressed:border-foreground" })}
        onclick={() => choose(item.id)}>
        <span aria-hidden="true" class="size-[var(--reddb-spatial-gap-md)] shrink-0 rounded-full" style:background={item.paint}></span>
        {item.label}<span class="text-ink-muted tabular-nums">{format(item.value)}</span>
      </button>
    {/each}
    {#if active}<button type="button" class={quietControl({ class: "min-h-[var(--reddb-spatial-control-height-sm)] text-caption underline" })} onclick={() => choose(null)}>Show all groups</button>{/if}
    {#if total === 0}<p class="m-0 self-center text-caption text-ink-muted">No contribution in this period.</p>{/if}
  </div>
</div>
