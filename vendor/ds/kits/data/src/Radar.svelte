<script module lang="ts">
  export type { VisualizationSize, VisualizationSeries } from "./visualization";
  export type { RadarAxis, RadarSeries } from "./radar";
</script>
<script lang="ts">
  import type { RadarContract } from "./core/contracts/radar";
  import Frame from "./internal/VisualizationFrame.svelte";
  import Mark from "./internal/VisualizationMark.svelte";
  import { edge, ink, mutedInk, surface, seriesColor, tint, number, short, type VisualizationSize } from "./visualization";
  import Canvas from "./internal/ChartCanvas.svelte";
  import { radarData, radarPoint, radarFraction, type RadarAxis, type RadarSeries } from "./radar";
  interface Props extends RadarContract {
    /** Reports selection for application-owned filtering or detail navigation. */
    onselectedchange?: (id: string) => void;
    /** Extra classes on the outer figure. */
    class?: string;
  }
  let { axes, data, label = "Radar", description, width, height, size = "lg", selected = $bindable(""), onselectedchange, interactive = true, detailHref, class: className = "" }: Props = $props();
  function select(id: string) { selected = id; onselectedchange?.(id); }
  const series = $derived(radarData(axes,data));
  const rows = $derived(series.map(s=>({...s,value:axes.map(a=>`${a.label}: ${s.values[a.id]??"unavailable"}`).join(" · ")})));
</script>
<Frame kind="Radar" {label} {description} {width} {height} {size} {selected} {detailHref} rows={rows} legend={series} onselect={interactive ? select : undefined} class={className} >
  {#snippet children({ width: w, height: h })}
    {@const cx=w/2} {@const cy=h/2} {@const radius=Math.max(20,Math.min(w/2-130,h/2-40))}
    <Canvas width={w} height={h} {label}>
      {#each [0.25,0.5,0.75,1] as fraction}
        <polygon points={axes.map((_,i)=>{const p=radarPoint(i,axes.length,fraction,cx,cy,radius);return `${p.x},${p.y}`;}).join(" ")} fill="none" stroke={edge}/>
      {/each}
      {#each axes as axis,i}
        {@const p=radarPoint(i,axes.length,1,cx,cy,radius)} {@const l=radarPoint(i,axes.length,1.15,cx,cy,radius)}
        <line x1={cx} y1={cy} x2={p.x} y2={p.y} stroke={edge}/>
        <text x={l.x} y={l.y} text-anchor={Math.abs(l.x-cx)<1?"middle":l.x>cx?"start":"end"} dominant-baseline="middle" fill={mutedInk} class="text-xs">{short(axis.label,22)}</text>
      {/each}
      {#each series as s (s.id)}
        {@const complete=axes.every(a=>s.values[a.id]!=null)}
        {@const path=axes.map((a,i)=>{const v=s.values[a.id];if(v==null)return "";const p=radarPoint(i,axes.length,radarFraction(v,a),cx,cy,radius);return `${i===0||s.values[axes[i-1]!.id]==null?"M":"L"}${p.x},${p.y}`;}).join(" ")+(complete?" Z":"")}
        <Mark id={s.id} label={rows.find(r=>r.id===s.id)!.value} {selected} onselect={interactive?select:undefined}>
          <path d={path} fill={complete?tint(seriesColor(s.series),12):"none"} fill-opacity={complete?0.5:0} stroke={seriesColor(s.series)} stroke-width={selected===s.id?3:1.5}/>
          {#each axes as a,i}{#if s.values[a.id]!=null}{@const p=radarPoint(i,axes.length,radarFraction(s.values[a.id]!,a),cx,cy,radius)}<circle cx={p.x} cy={p.y} r={3} fill={seriesColor(s.series)}/>{/if}{/each}
        </Mark>
      {/each}
    </Canvas>
  {/snippet}
</Frame>
