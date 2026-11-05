<script lang="ts">
  import type { ComponentProps, Snippet } from "svelte";
  import { ChartCore as Chart, Svg } from "layerchart/svg";
  let { width, height, label, data = [], padding = 0, children }: { width: number; height: number; label: string; data?: ComponentProps<typeof Chart>["data"]; padding?: number | {left:number;right:number;top:number;bottom:number}; children: Snippet } = $props();
</script>
<Chart ssr {width} {height} {data} {padding} axis={false} grid={false} rule={false} highlight={false} tooltipContext={false}>
  <Svg title={label} role="group" aria-label={label}>
    {@render children()}
  </Svg>
</Chart>
