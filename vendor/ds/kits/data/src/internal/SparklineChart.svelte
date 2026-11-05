<!-- Internal: the Sparkline drawing, loaded lazily by Sparkline.svelte (wave 5D). -->
<script lang="ts">
  import { Area, Chart, Rect, Spline, Svg } from "layerchart/svg";
  import { curveLinear, curveMonotoneX } from "d3-shape";
  import type { ChartCurve } from "../chart";
  import type { SparklineVariant } from "../Sparkline.svelte";

  interface Props {
    /** Points to plot, as `{ x, y }`. */
    data: readonly { x: number; y: number }[];
    curve: ChartCurve;
    variant: SparklineVariant;
    /** Drawing width in px. */
    width: number;
    /** Drawing height in px. */
    height: number;
  }

  const { data, curve, variant, width, height }: Props = $props();
  const observations = $derived(data.filter(point => Number.isFinite(point.y)));
  const interpolation = $derived(curve === "smooth" ? curveMonotoneX : curveLinear);
  const zeroDomain = $derived.by((): [number, number] => {
    let low = 0, high = 0;
    for (const point of observations) { low = Math.min(low, point.y); high = Math.max(high, point.y); }
    return low === high ? [0, 1] : [low, high];
  });
  const columns = $derived(observations.map(point => ({ x: point.x - 0.36, x1: point.x + 0.36, y: 0, y1: point.y })));
</script>

<Chart
  ssr
  data={observations}
  x="x"
  y="y"
  xDomain={variant === "bar" ? [-0.5, Math.max(0.5, data.length - 0.5)] : undefined}
  xNice={false}
  yDomain={variant === "line" && observations.length ? undefined : zeroDomain}
  {width}
  {height}
  pointerEvents={false}
  tooltipContext={false}
  padding={2}
>
  <Svg pointerEvents={false}>
    {#if variant === "bar"}
      <Rect data={columns} x0="x" x1="x1" y0="y" y1="y1" fill="var(--color-primary)" data-sparkline-column />
    {:else}
      {#if variant === "area"}<Area data={data as { x: number; y: number }[]} y0={() => 0} defined={point => Number.isFinite(point.y)} curve={interpolation} fill="var(--color-primary)" fillOpacity={0.25} data-sparkline-area />{/if}
      <Spline data={data as { x: number; y: number }[]} defined={point => Number.isFinite(point.y)} curve={interpolation} stroke="var(--color-primary)" strokeWidth={2} />
    {/if}
  </Svg>
</Chart>
