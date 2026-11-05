<script module lang="ts">
  export type { VisualizationSize, VisualizationSeries } from "./visualization";
  export type { ScatterPoint } from "./scatter";
</script>
<script lang="ts">
  import type { ScatterContract } from "./core/contracts/scatter";
  import Frame from "./internal/VisualizationFrame.svelte";
  import Mark from "./internal/VisualizationMark.svelte";
  import { edge, ink, mutedInk, surface, seriesColor, tint, number, short, type VisualizationSize } from "./visualization";
  import Canvas from "./internal/ChartCanvas.svelte";
  import { scatterData, regression, type ScatterPoint } from "./scatter";
  import { domain, project, ticks } from "./visualization";
  interface Props extends ScatterContract {
    /** Reports selection for application-owned filtering or detail navigation. */
    onselectedchange?: (id: string) => void;
    /** Extra classes on the outer figure. */
    class?: string;
  }
  let { data, xDomain, yDomain, xLabel = "", yLabel = "", trend = false, formatX = number, formatY = number, label = "Scatter", description, width, height, size = "lg", selected = $bindable(""), onselectedchange, interactive = true, detailHref, class: className = "" }: Props = $props();
  function select(id: string) { selected = id; onselectedchange?.(id); }
  const points = $derived(scatterData(data));
  const xd = $derived(domain(points.map(p => p.x), xDomain));
  const yd = $derived(domain(points.map(p => p.y), yDomain));
  const line = $derived(trend ? regression(points) : undefined);
  const rows = $derived(points.map(p => ({ ...p, value: `${formatX(p.x)} / ${formatY(p.y)}` })));
</script>
<Frame kind="Scatter" {label} {description} {width} {height} {size} {selected} {detailHref} rows={rows} onselect={interactive ? select : undefined} class={className} >
  {#snippet children({ width: w, height: h })}
    {@const left = Math.min(w * 0.2, Math.max(44, ...ticks(yd).map(v => formatY(v).length * 7 + 12)))}
    <Canvas width={w} height={h} {label}>
      {#each ticks(yd) as tick}
        {@const y = project(tick, yd, h - 44, 28)}
        <line x1={left} x2={w-20} y1={y} y2={y} stroke={edge}/>
        <text x={left-8} {y} text-anchor="end" dominant-baseline="middle" fill={mutedInk} class="font-mono text-xs">{formatY(tick)}</text>
      {/each}
      {#each ticks(xd) as tick}
        <text x={project(tick,xd,left,w-20)} y={h-24} text-anchor="middle" fill={mutedInk} class="font-mono text-xs">{formatX(tick)}</text>
      {/each}
      <text x={left} y={16} fill={mutedInk} class="text-xs">{yLabel}</text>
      <text x={w-20} y={h-5} text-anchor="end" fill={mutedInk} class="text-xs">{xLabel}</text>
      {#if line}<line x1={project(line[0]!.x,xd,left,w-20)} y1={project(line[0]!.y,yd,h-44,28)} x2={project(line[1]!.x,xd,left,w-20)} y2={project(line[1]!.y,yd,h-44,28)} stroke={mutedInk} stroke-dasharray="5 5"/>{/if}
      {#each points as p (p.id)}
        <Mark id={p.id} label={`${p.label}: ${formatX(p.x)}, ${formatY(p.y)}`} {selected} onselect={interactive ? select : undefined}>
          <circle cx={project(p.x,xd,left,w-20)} cy={project(p.y,yd,h-44,28)} r={p.radius??5} fill={tint(seriesColor(p.series),selected===p.id?55:20)} stroke={seriesColor(p.series)} stroke-width={selected===p.id?3:1.5}/>
        </Mark>
      {/each}
    </Canvas>
  {/snippet}
</Frame>
