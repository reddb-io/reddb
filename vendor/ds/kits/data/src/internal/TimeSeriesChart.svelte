<!-- Internal: the TimeSeries drawing, loaded lazily by TimeSeries.svelte (wave 5D). -->
<script lang="ts">
  import { Area, Axis, Chart, Grid, Rect, Spline, Svg } from "layerchart/svg";
  import { curveLinear, curveMonotoneX } from "d3-shape";
  import { formatTimeTick, timeSeriesAreas, timeSeriesBars, timeSeriesDateTicks } from "../time-series";
  import type { ChartCurve, ChartNumberFormat } from "../chart";
  import type { ResolvedTimeSeries } from "../time-series";

  interface Props {
    /** Points to plot, as `{ x, y }`. */
    data: readonly { x: number | Date; y: number }[];
    datasets: readonly ResolvedTimeSeries[];
    domain: [number, number];
    samples: readonly (number | Date)[];
    valueDomain?: [number, number];
    cursor?: number | Date;
    formatX?: (value: number | Date) => string;
    /** `area` fills to zero; `line` reads the data's own range. */
    variant: "line" | "area" | "stacked-area" | "bar";
    curve: ChartCurve;
    presentation: "full" | "minimal";
    barDomain: [number, number];
    /** Value-axis tick formatter. */
    format: ChartNumberFormat;
    /** Zero for an area, none for a line. */
    baseline: number | undefined;
    /** Drawing width in px. */
    width: number;
    /** Drawing height in px. */
    height: number;
    /** Plot padding in px. */
    padding: { top: number; right: number; bottom: number; left: number };
  }

  const { data, datasets, domain, samples, valueDomain, barDomain, cursor, formatX, variant, curve, presentation, format, baseline, width, height, padding }: Props = $props();
  const bars = $derived(timeSeriesBars(datasets, samples));
  const areas = $derived(timeSeriesAreas(datasets, samples));
  const interpolation = $derived(curve === "smooth" ? curveMonotoneX : curveLinear);
  const dateTicks = $derived(timeSeriesDateTicks(samples, (width - padding.left - padding.right) / Math.max(padding.left, 1)));
  const cursorPixel = $derived(cursor === undefined ? undefined : (+cursor - domain[0]) / (domain[1] - domain[0]) * (width - padding.left - padding.right));
</script>

<Chart
  ssr
  data={data as { x: number | Date; y: number }[]}
  x="x"
  y="y"
  yBaseline={baseline}
  xDomain={domain}
  xNice={false}
  yDomain={valueDomain ?? (variant === "bar" || variant === "stacked-area" ? barDomain : data.length === 0 ? [0, 1] : undefined)}
  yNice={valueDomain === undefined}
  {width}
  {height}
  tooltipContext={false}
  {padding}
>
  <Svg>
    {#if presentation === "full"}<Grid y />{/if}
    {#each datasets as dataset (dataset.id)}
      <g data-chart-line={dataset.id}>
        {#if variant === "bar"}
          <Rect data={bars.find(series => series.id === dataset.id)?.data ?? []} x0="x" x1="x1" y0="y" y1="y1" fill={dataset.paint} data-chart-column />
        {:else if variant === "stacked-area"}
          {@const band = areas.find(series => series.id === dataset.id)?.data ?? []}
          <Area data={band} x="x" y0="y" y1="y1" defined={point => point.defined} curve={interpolation} fill={dataset.paint} fillOpacity={0.7} data-chart-area={dataset.id} />
          <Spline data={band} x="x" y="y1" defined={point => point.defined} curve={interpolation} stroke={dataset.paint} strokeWidth={2} />
        {:else if variant === "area"}
          <Area data={dataset.data} y0={() => 0} curve={interpolation} fill={dataset.paint} fillOpacity={0.3} />
        {/if}
        {#if variant !== "bar" && variant !== "stacked-area"}<Spline data={dataset.data} curve={interpolation} stroke={dataset.paint} strokeWidth={2} />{/if}
      </g>
    {/each}
    {#if cursorPixel !== undefined}
      <line data-chart-cursor x1={cursorPixel} x2={cursorPixel} y1={0} y2={height - padding.top - padding.bottom} stroke="var(--reddb-color-ink-muted)" stroke-dasharray="3 3" />
    {/if}
    {#if presentation === "full"}
      <Axis placement="left" {format} tickLength={2} tickMarks={false} />
      <Axis placement="bottom" ticks={dateTicks} tickOcclusion format={formatX ?? (dateTicks ? value => formatTimeTick(value, dateTicks) : undefined)} />
    {/if}
  </Svg>
</Chart>
