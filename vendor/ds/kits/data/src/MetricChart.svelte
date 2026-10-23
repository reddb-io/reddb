<script lang="ts">
 import type {MetricChartContract} from "./core/contracts/metricchart";
  import { Button, Dialog, Icon, quietControl, type IconGlyph } from "@reddb-io/design-system/base";
  import TimeSeries, { type TimeSeriesDataset, type TimeSeriesVariant } from "./TimeSeries.svelte";
  import Table, { type TableCellContext } from "./Table.svelte";
  import { formatChartNumber, type ChartCurve, type ChartNumberFormat, type ChartSize } from "./chart";
  import { timeSeriesDatasets } from "./time-series";
  import { metricStatistics } from "./metric";

  interface Props extends MetricChartContract {
    /** Reports opening and closing the expanded view. */
    onexpandedchange?: (open: boolean) => void;
    /** Reports legend changes from either view. */
    onhiddenserieschange?: (ids: readonly string[]) => void;
    /** Extra classes merged on the metric surface. */
    class?: string;
  }

  let { label, data, description, aggregate = "total", format = formatChartNumber, formatX,
    variant = "bar", curve = "smooth", presentation = "summary", size, height, detailHref, detailLabel = "Explore",
    expandable = true, expandIcon, expanded = $bindable(false), onexpandedchange,
    hiddenSeries = $bindable([]), onhiddenserieschange, class: className = "" }: Props = $props();

  const resolved = $derived(timeSeriesDatasets(data, label, 1));
  const statistics = $derived(metricStatistics(resolved));
  const observed = $derived(statistics.filter(row => row.count > 0));
  const headline = $derived(observed.length ? aggregate === "average"
    ? observed.reduce((sum, row) => sum + (row.total ?? 0), 0) / observed.reduce((sum, row) => sum + row.count, 0)
    : observed.reduce((sum, row) => sum + (row[aggregate] ?? 0), 0) : null);
  const reading = (value: unknown) => typeof value === "number" ? format(value) : "No samples";
  const columns = $derived([
    { key: "label", header: "Series", sortable: true },
    { key: "min", header: "Min", sortable: true, align: "end" as const, format: reading },
    { key: "max", header: "Max", sortable: true, align: "end" as const, format: reading },
    { key: "average", header: "Average", sortable: true, align: "end" as const, format: reading },
    { key: "total", header: "Total", sortable: true, align: "end" as const, format: reading },
  ]);
  function toggle(id: string): void {
    hiddenSeries = hiddenSeries.includes(id) ? hiddenSeries.filter(value => value !== id) : [...hiddenSeries, id];
    onhiddenserieschange?.(hiddenSeries);
  }
  function openDetails(): void { expanded = true; onexpandedchange?.(true); }
</script>

{#snippet seriesCell({ row, column, text }: TableCellContext)}
  {#if column.key === "label"}
    <span class="inline-flex items-center gap-[var(--reddb-spatial-gap-sm)]">
      <span aria-hidden="true" class="size-[var(--reddb-spatial-gap-md)] shrink-0 rounded-full" style:background={String(row.paint)}></span>{text}
    </span>
  {:else}{text}{/if}
{/snippet}

<section data-metric-chart data-metric-presentation={presentation} aria-label={label}
  class={`flex min-w-0 flex-col gap-[var(--reddb-spatial-layout-gap-sm)] rounded-lg border border-muted bg-elevation-raised-surface p-[var(--reddb-spatial-inset-md)] text-foreground ${className}`}>
  <div class="flex min-w-0 items-start justify-between gap-[var(--reddb-spatial-gap-md)]">
    <div class="min-w-0">
      <p class="m-0 text-caption text-ink-muted">{label}</p>
      <p data-metric-value class="m-0 text-title tabular-nums">{reading(headline)}</p>
      {#if description}<p class="m-0 text-caption text-ink-muted">{description}</p>{/if}
    </div>
    <div class="flex shrink-0 items-center gap-[var(--reddb-spatial-gap-sm)]">
      {#if detailHref}<Button href={detailHref} variant="ghost" size="sm">{detailLabel}</Button>{/if}
      {#if expandable}<Button variant="ghost" size="sm" aria-label={`Expand ${label}`} title={`Expand ${label}`} onclick={openDetails}>{#if expandIcon}<Icon icon={expandIcon} size="sm" />{:else}Expand{/if}</Button>{/if}
    </div>
  </div>
  <TimeSeries {data} {label} {variant} {curve} {format} {formatX} {height} size={size ?? (presentation === "summary" ? "sm" : "md")}
    presentation={presentation === "summary" ? "minimal" : "full"} interactive={presentation === "full"}
    bind:hiddenSeries {onhiddenserieschange} class="p-0" />
  {#if presentation === "summary"}
    <div data-metric-legend class="grid gap-[var(--reddb-spatial-gap-sm)]" aria-label={`${label} series`}>
      {#each statistics as row (row.id)}
        <button type="button" aria-pressed={!hiddenSeries.includes(row.id)} data-metric-series={row.id}
          class={quietControl({ ink: "foreground", class: "flex min-w-0 min-h-[var(--reddb-spatial-control-height-sm)] items-center gap-[var(--reddb-spatial-gap-md)] rounded-md text-caption" })}
          onclick={() => toggle(row.id)}>
          <span aria-hidden="true" class={`size-[var(--reddb-spatial-gap-md)] shrink-0 rounded-full border border-current ${hiddenSeries.includes(row.id) ? "bg-transparent" : "bg-current"}`} style:color={row.paint}></span>
          <span class={`min-w-0 flex-1 text-start ${hiddenSeries.includes(row.id) ? "text-ink-muted" : ""}`}>{row.label}</span>
          <span class="shrink-0 text-ink-muted tabular-nums">{reading(row[aggregate])}</span>
        </button>
      {/each}
      {#if statistics.length === 0}<p class="m-0 text-caption text-ink-muted">No samples in this period.</p>{/if}
      {#if statistics.length > 0 && statistics.every(row => hiddenSeries.includes(row.id))}<p class="m-0 text-caption text-ink-muted">All series hidden. Use the legend to restore them.</p>{/if}
    </div>
  {/if}
</section>

<Dialog title={label} description={description} size="xl" bind:open={expanded} onopenchange={onexpandedchange}>
  {#if expanded}
    <div data-metric-detail class="grid min-w-0 gap-[var(--reddb-spatial-layout-gap-md)]">
      <TimeSeries {data} {label} {variant} {curve} {format} {formatX} size="lg" bind:hiddenSeries {onhiddenserieschange} />
      <Table caption={`${label}: observed sample statistics`} {columns} rows={statistics} rowKey="id" cell={seriesCell} />
    </div>
  {/if}
</Dialog>
