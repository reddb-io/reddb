<script module lang="ts">
  export type { StripMetric } from "./core/contracts/metricstrip";
</script>

<script lang="ts">
  import KpiCard from "./KpiCard.svelte";
  import Sparkline from "./Sparkline.svelte";
  import { uniqueEntities } from "./operational";
  import { operationalView } from "./operational.variants";
  import type { MetricStripContract } from "./core/contracts/metricstrip";
  interface Props extends MetricStripContract {
    /** Additional root classes. */
    class?: string;
  }
  let { label, items, class: className }: Props = $props();
  const s = operationalView();
</script>

<div
  data-metric-strip
  role="group"
  aria-label={label}
  class={s.metrics({ class: className })}
>
  {#each uniqueEntities(items, "MetricStrip") as metric (metric.id)}{#if metric.values?.length}<KpiCard
        label={metric.label}
        value={metric.value}
        description={metric.description}
        delta={metric.delta}
        polarity={metric.polarity}
        >{#snippet sparkline()}<Sparkline
            values={metric.values ?? []}
            variant={metric.variant ?? "line"}
            label={`${metric.label} trend`}
          />{/snippet}</KpiCard
      >{:else}<KpiCard
        label={metric.label}
        value={metric.value}
        description={metric.description}
        delta={metric.delta}
        polarity={metric.polarity}
      />{/if}{/each}
</div>
