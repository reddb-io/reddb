<!-- Raw numeric distributions binned by the DS and rendered by LayerChart. -->
<script module lang="ts">
  export * from "./core/histogram";
</script>

<script lang="ts">
  import { type HistogramBin, histogramBins } from "./core/histogram";
  import type { HistogramContract } from "./core/contracts/histogram";
  import { onMount } from "svelte";
  import {
    CATEGORY_AXIS_GUTTER_PX,
    CHART_ROWS,
    COMFORTABLE_CHART_RHYTHM,
    bandLabelCount,
    observeChartRhythm,
    valueAxisGutter,
    type ChartSeries,
    type ChartSize,
  } from "./chart";
  import { chartFigure } from "./chart.variants";
  import { histogramEngine } from "./chart-engines";
  import { lazyChart } from "./chart-loader.svelte";
  import type { ChartLoading } from "./chart-loading";

  interface Props extends HistogramContract {
    /** Extra classes, merged onto the chart's root `figure`. */
    class?: string;
  }

  let {
    values,
    bins = 10,
    label = "Histogram",
    format,
    series = 1,
    width,
    height,
    size = "lg",
    loading = "lazy",
    class: className = "",
  }: Props = $props();

  const FALLBACK_WIDTH = 640;
  const ROWS = $derived(CHART_ROWS[size]);
  // Space between bars, as a fraction of each bin: enough to read the bins
  // apart while the distribution still reads as one continuous shape.
  const BAND_PADDING = 0.08;
  // LayerChart's own spacing between ticks on a vertical axis.
  const COUNT_TICK_SPACING = 50;
  let box = $state<DOMRectReadOnly>();
  let rhythm = $state(COMFORTABLE_CHART_RHYTHM);
  let probe: HTMLElement;
  let figure = $state<HTMLElement>();
  const chart = lazyChart(histogramEngine, () => figure, () => loading);
  onMount(() => observeChartRhythm(probe, (next) => (rhythm = next)));

  const data = $derived(histogramBins(values, bins, format));
  const largestCount = $derived(data.reduce((largest, bin) => Math.max(largest, bin.count), 0));
  const drawingWidth = $derived(
    width ?? (box && box.width > 0 ? Math.floor(box.width) : FALLBACK_WIDTH),
  );
  const drawingHeight = $derived(height ?? Math.round(rhythm.row * ROWS));
  const padding = $derived({
    top: rhythm.inset,
    right: rhythm.inset,
    bottom: CATEGORY_AXIS_GUTTER_PX,
    left: valueAxisGutter([String(largestCount)]),
  });
  // Counts are whole: the value axis keeps only integer ticks, and the bin axis
  // only as many labels as fit across the plot without colliding.
  const countTickCount = $derived(
    Math.max(2, Math.round((drawingHeight - padding.top - padding.bottom) / COUNT_TICK_SPACING)),
  );
  const countTicks = (scale: { ticks?: (count?: number) => unknown[] }): number[] =>
    (scale.ticks?.(countTickCount) ?? []).filter(
      (tick): tick is number => typeof tick === "number" && Number.isInteger(tick),
    );
  const binTicks = $derived(
    bandLabelCount(
      data.map((bin) => bin.label),
      drawingWidth - padding.left - padding.right,
    ),
  );
</script>

<figure
  bind:this={figure}
  data-ds-chart
  data-chart-loaded={chart.Engine ? "" : undefined}
  data-chart-kind="histogram"
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
    <Engine
      {data}
      width={drawingWidth}
      height={drawingHeight}
      {padding}
      bandPadding={BAND_PADDING}
      {countTicks}
      {binTicks}
    />
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
