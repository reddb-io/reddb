<script module lang="ts">
  export type { VisualizationSize, VisualizationSeries } from "./visualization";
  export type { WaterfallStep } from "./waterfall";
</script>
<script lang="ts">
  import type { WaterfallContract } from "./core/contracts/waterfall";
  import Frame from "./internal/VisualizationFrame.svelte";
  import Mark from "./internal/VisualizationMark.svelte";
  import { edge, ink, mutedInk, surface, seriesColor, tint, number, short, type VisualizationSize } from "./visualization";
  import Canvas from "./internal/ChartCanvas.svelte";
  import { waterfallSteps, type WaterfallStep } from "./waterfall";
  import { domain, project, ticks } from "./visualization";
  interface Props extends WaterfallContract {
    /** Reports selection for application-owned filtering or detail navigation. */
    onselectedchange?: (id: string) => void;
    /** Extra classes on the outer figure. */
    class?: string;
  }
  let { data, format = number, label = "Waterfall", description, width, height, size = "lg", selected = $bindable(""), onselectedchange, interactive = true, detailHref, class: className = "" }: Props = $props();
  function select(id: string) { selected = id; onselectedchange?.(id); }
  const steps=$derived(waterfallSteps(data));
  const yd=$derived(domain([0,...steps.flatMap(s=>[s.start,s.end])]));
  const rows=$derived(steps.map(s=>({...s,value:format(s.amount),description:`${s.kind} · balance ${format(s.end)}`})));
</script>
<Frame kind="Waterfall" {label} {description} {width} {height} {size} {selected} {detailHref} rows={rows} onselect={interactive ? select : undefined} class={className} >
  {#snippet children({ width: w, height: h })}
    {@const left=Math.min(w*.2,Math.max(44,...ticks(yd).map(v=>format(v).length*7+12)))} {@const bottom=h-50} {@const band=(w-left-12)/Math.max(1,steps.length)}
    <Canvas width={w} height={h} {label}>
      {#each ticks(yd) as tick}{@const y=project(tick,yd,bottom,28)}<line x1={left} x2={w-12} y1={y} y2={y} stroke={edge}/><text x={left-8} {y} text-anchor="end" dominant-baseline="middle" fill={mutedInk} class="font-mono text-xs">{format(tick)}</text>{/each}
      {#each steps as step,i}
        {@const x=left+i*band+band*.13} {@const y1=project(step.start,yd,bottom,28)} {@const y2=project(step.end,yd,bottom,28)}
        <Mark id={step.id} label={`${step.label}: ${format(step.amount)}, balance ${format(step.end)}`} {selected} onselect={interactive?select:undefined}>
          <rect {x} y={Math.min(y1,y2)} width={band*.74} height={Math.max(1,Math.abs(y2-y1))} fill={step.kind!=="delta"?tint(seriesColor(step.series),30):step.amount<0?"var(--reddb-color-background)":tint(seriesColor(step.series),14)} stroke={selected===step.id?ink:seriesColor(step.series)} stroke-width={selected===step.id?2.5:1.5}/>
          <text x={x+band*.37} y={Math.min(y1,y2)-8} text-anchor="middle" fill={ink} class="font-mono text-xs">{step.kind==="delta"&&step.amount>0?"+":""}{format(step.amount)}</text>
          <text x={x+band*.37} y={bottom+20} text-anchor="middle" fill={mutedInk} class="text-xs">{short(step.label,Math.max(5,Math.floor(band/7)))}</text>
        </Mark>
        {#if i<steps.length-1}<line x1={x+band*.74} x2={x+band} y1={y2} y2={y2} stroke={mutedInk}/>{/if}
      {/each}
    </Canvas>
  {/snippet}
</Frame>
