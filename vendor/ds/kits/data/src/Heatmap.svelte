<!-- Calendar heatmap composed from LayerChart's SVG Chart and Calendar marks. -->
<script module lang="ts">
  export * from "./core/heatmap";
</script>

<script lang="ts">
  import { type HeatmapPoint } from "./core/heatmap";
  import type { HeatmapContract } from "./core/contracts/heatmap";
  import { onMount } from "svelte";
  import {
    COMFORTABLE_CHART_RHYTHM,
    heatmapStep,
    observeChartRhythm,
    type ChartSeries,
  } from "./chart";
  import { chartFigure } from "./chart.variants";
  import { heatmapEngine } from "./chart-engines";
  import { lazyChart } from "./chart-loader.svelte";
  import type { ChartLoading } from "./chart-loading";

  interface Props extends HeatmapContract {
    /** Extra classes, merged onto the chart's root `figure`. */
    class?: string;
  }

  let {
    data,
    start,
    end,
    label = "Heatmap",
    series = 1,
    width,
    height,
    loading = "lazy",
    class: className = "",
  }: Props = $props();

  const FALLBACK_WIDTH = 640;
  const ROWS = 5;
  let box = $state<DOMRectReadOnly>();
  let rhythm = $state(COMFORTABLE_CHART_RHYTHM);
  let probe: HTMLElement;
  let figure = $state<HTMLElement>();
  const chart = lazyChart(heatmapEngine, () => figure, () => loading);
  onMount(() => observeChartRhythm(probe, (next) => (rhythm = next)));

  const drawingWidth = $derived(
    width ?? (box && box.width > 0 ? Math.floor(box.width) : FALLBACK_WIDTH),
  );
  const drawingHeight = $derived(height ?? Math.round(rhythm.row * ROWS));

  // LayerChart's Calendar lays days out in the viewer's local calendar, so a
  // point belongs to the local day it falls on, whatever its time of day, and
  // points sharing a day add up. The ramp's top step is the busiest day shown.
  const dayOf = (date: Date): number =>
    new Date(date.getFullYear(), date.getMonth(), date.getDate()).valueOf();
  const valueByDay = $derived.by(() => {
    const totals = new Map<number, number>();
    for (const point of data) {
      if (!Number.isFinite(point.value) || Number.isNaN(point.date.valueOf())) continue;
      const day = dayOf(point.date);
      totals.set(day, (totals.get(day) ?? 0) + point.value);
    }
    return totals;
  });
  const maximum = $derived.by(() => {
    const [first, last] = [dayOf(start), end.valueOf()];
    let largest = 0;
    for (const [day, value] of valueByDay) {
      if (day >= first && day < last) largest = Math.max(largest, value);
    }
    return largest;
  });
  const stepOf = (date: Date): number => heatmapStep(valueByDay.get(dayOf(date)), maximum);
</script>

<figure
  bind:this={figure}
  data-ds-chart
  data-chart-loaded={chart.Engine ? "" : undefined}
  data-chart-kind="heatmap"
  data-chart-series={series}
  data-chart-width={width === undefined ? "fluid" : "fixed"}
  bind:contentRect={box}
  role="img"
  aria-label={label}
  class={chartFigure({ series, fluid: width === undefined, class: `w-full overflow-hidden rounded-lg ${className}` })}
>
  <span
    bind:this={probe}
    data-chart-rhythm-probe
    aria-hidden="true"
    class="pointer-events-none invisible absolute start-0 top-0 h-[var(--reddb-spatial-control-height-md)] w-[var(--reddb-spatial-inset-sm)]"
  ></span>
  {#if chart.Engine}
    {@const Engine = chart.Engine}
    <Engine {start} {end} {stepOf} width={drawingWidth} height={drawingHeight} />
  {:else}
    <!-- The drawing's own height before it exists: `height`, or ROWS of the
         Density's control height — the length the probe reads once mounted —
         so the swap to the engine moves nothing (CLS 0) at every stop. -->
    <div
      data-chart-placeholder
      aria-hidden="true"
      class="w-full"
      style:height={height === undefined ? `calc(var(--reddb-spatial-control-height-md) * ${ROWS})` : `${height}px`}
    ></div>
  {/if}
</figure>
