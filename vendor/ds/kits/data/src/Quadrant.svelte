<script module lang="ts">
  export type { VisualizationSize, VisualizationSeries } from "./visualization";
  export type { ScatterPoint as QuadrantPoint } from "./scatter";
</script>
<script lang="ts">
  import type { QuadrantContract } from "./core/contracts/quadrant";
  import Frame from "./internal/VisualizationFrame.svelte";
  import Mark from "./internal/VisualizationMark.svelte";
  import { edge, ink, mutedInk, surface, seriesColor, tint, number, short, type VisualizationSize } from "./visualization";
  import Canvas from "./internal/ChartCanvas.svelte";
  import { scatterData, type ScatterPoint } from "./scatter";
  import { domain, project } from "./visualization";
  interface Props extends QuadrantContract {
    /** Reports selection for application-owned filtering or detail navigation. */
    onselectedchange?: (id: string) => void;
    /** Extra classes on the outer figure. */
    class?: string;
  }
  let { data, xDomain = [0, 100], yDomain = [0, 100], thresholds, regions = ["High / Low", "High / High", "Low / Low", "Low / High"], xLabel = "", yLabel = "", label = "Quadrant", description, width, height, size = "lg", selected = $bindable(""), onselectedchange, interactive = true, detailHref, class: className = "" }: Props = $props();
  function select(id: string) { selected = id; onselectedchange?.(id); }
  const points = $derived(scatterData(data));
  const xd = $derived(domain([],xDomain)), yd = $derived(domain([],yDomain));
  const split = $derived(thresholds??[(xd[0]+xd[1])/2,(yd[0]+yd[1])/2]);
  const rows = $derived(points.map(p=>({...p,value:`${number(p.x)} / ${number(p.y)}`})));
</script>
<Frame kind="Quadrant" {label} {description} {width} {height} {size} {selected} {detailHref} rows={rows} onselect={interactive ? select : undefined} class={className} >
  {#snippet children({ width: w, height: h })}
    {@const left=24} {@const right=w-24} {@const top=34} {@const bottom=h-36}
    {@const sx=project(split[0]!,xd,left,right)} {@const sy=project(split[1]!,yd,bottom,top)}
    <Canvas width={w} height={h} {label}>
      <line x1={sx} x2={sx} y1={top} y2={bottom} stroke={edge}/><line x1={left} x2={right} y1={sy} y2={sy} stroke={edge}/>
      {#each regions as region,i}<text x={i%2===0?left+8:right-8} y={i<2?top+12:bottom-10} text-anchor={i%2===0?"start":"end"} fill={mutedInk} class="font-mono text-xs">{region}</text>{/each}
      <text x={left} y={16} fill={mutedInk} class="text-xs">{yLabel}</text><text x={right} y={h-8} text-anchor="end" fill={mutedInk} class="text-xs">{xLabel}</text>
      {#each points as p (p.id)}
        {@const x=project(p.x,xd,left,right)} {@const y=project(p.y,yd,bottom,top)}
        <Mark id={p.id} label={`${p.label}: ${number(p.x)} / ${number(p.y)}`} {selected} onselect={interactive?select:undefined}>
          <circle cx={x} cy={y} r={selected===p.id?6:4} fill={seriesColor(p.series)}/>
          <text x={x> w*0.7?x-12:x+12} {y} text-anchor={x>w*0.7?"end":"start"} dominant-baseline="middle" fill={selected===p.id?ink:mutedInk} class="text-xs">{short(p.label,24)}</text>
        </Mark>
      {/each}
    </Canvas>
  {/snippet}
</Frame>
