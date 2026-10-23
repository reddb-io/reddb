<!-- Internal: the Histogram drawing, loaded lazily by Histogram.svelte (wave 5D). -->
<script lang="ts">
  import { Axis, Bars, Chart, Grid, Svg } from "layerchart/svg";
  import { formatChartNumber } from "../chart";

  interface Props {
    /** The binned counts. */
    data: readonly { x0: number; x1: number; label: string; count: number }[];
    /** Drawing width in px. */
    width: number;
    /** Drawing height in px. */
    height: number;
    /** Plot padding in px. */
    padding: { top: number; right: number; bottom: number; left: number };
    /** Space between bars, as a fraction of each bin. */
    bandPadding: number;
    /** Whole-number count ticks for the value axis and grid. */
    countTicks: (scale: { ticks?: (count?: number) => unknown[] }) => number[];
    /** How many bin labels fit across the plot. */
    binTicks: number;
  }

  const { data, width, height, padding, bandPadding, countTicks, binTicks }: Props = $props();
  const labelByEdge = $derived(new Map(data.map((bin) => [bin.x0, bin.label])));
  const formatCount = (count: number): string => formatChartNumber(count, 0);
</script>

<Chart
  ssr
  data={data as { x0: number; x1: number; label: string; count: number }[]}
  x="x0"
  y="count"
  valueAxis="y"
  {bandPadding}
  yBaseline={0}
  yNice
  {width}
  {height}
  tooltipContext={false}
  {padding}
>
  <Svg>
    <Grid y yTicks={countTicks} />
    <Bars fill="var(--color-primary)" />
    <Axis placement="left" ticks={countTicks} format={formatCount} />
    <Axis
      placement="bottom"
      ticks={binTicks}
      format={(edge: number) => labelByEdge.get(edge) ?? formatChartNumber(edge)}
    />
  </Svg>
</Chart>
