<!-- Internal: the Heatmap drawing, loaded lazily by Heatmap.svelte (wave 5D). -->
<script lang="ts">
  import { Calendar, Chart, Rect, Svg } from "layerchart/svg";
  import { HEATMAP_FILLS } from "../chart";

  interface Props {
    /** Inclusive first calendar day. */
    start: Date;
    /** Exclusive end of the calendar range. */
    end: Date;
    /** The intensity step (0–4) of the local day a cell date falls on. */
    stepOf: (date: Date) => number;
    /** Drawing width in px. */
    width: number;
    /** Drawing height in px. */
    height: number;
  }

  const { start, end, stepOf, width, height }: Props = $props();
</script>

<Chart
  ssr
  {width}
  {height}
  axis={false}
  grid={false}
  rule={false}
  highlight={false}
  tooltipContext={false}
  padding={2}
>
  <Svg>
    <Calendar {start} {end} monthLabel={false} monthPath={false}>
      {#snippet children({ cells, cellSize })}
        {#each cells as cell (cell.data.date.valueOf())}
          {@const step = stepOf(cell.data.date)}
          <Rect
            x={cell.x}
            y={cell.y}
            width={cellSize[0]}
            height={cellSize[1]}
            fill={HEATMAP_FILLS[step]}
            strokeWidth={1}
            class="lc-calendar-cell"
            data-heatmap-step={step}
          />
        {/each}
      {/snippet}
    </Calendar>
  </Svg>
</Chart>
