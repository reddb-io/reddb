<script module lang="ts">
  export type { VisualizationSize, VisualizationSeries } from "./visualization";
  export type { MatrixAxis, MatrixCell } from "./matrix-heatmap";
</script>
<script lang="ts">
  import type { MatrixHeatmapContract } from "./core/contracts/matrixheatmap";
  import Frame from "./internal/VisualizationFrame.svelte";
  import Mark from "./internal/VisualizationMark.svelte";
  import { edge, ink, mutedInk, surface, seriesColor, tint, number, short, type VisualizationSize } from "./visualization";
  import Canvas from "./internal/ChartCanvas.svelte";
  import { matrixData, type MatrixAxis, type MatrixCell } from "./matrix-heatmap";
  import { domain, project } from "./visualization";
  interface Props extends MatrixHeatmapContract {
    /** Reports selection for application-owned filtering or detail navigation. */
    onselectedchange?: (id: string) => void;
    /** Extra classes on the outer figure. */
    class?: string;
  }
  let { rowAxis, columnAxis, data, valueDomain, format = number, label = "MatrixHeatmap", description, width, height, size = "lg", selected = $bindable(""), onselectedchange, interactive = true, detailHref, class: className = "" }: Props = $props();
  function select(id: string) { selected = id; onselectedchange?.(id); }
  const cells=$derived(matrixData(rowAxis,columnAxis,data));
  const scale=$derived(domain([0,...cells.flatMap(c=>c.value===null?[]:[c.value])],valueDomain));
  const rows=$derived(cells.map(c=>({...c,value:c.value===null?"Unavailable":format(c.value)})));
</script>
<Frame kind="MatrixHeatmap" {label} {description} {width} {height} {size} {selected} {detailHref} rows={rows} onselect={interactive ? select : undefined} class={className} >
  {#snippet children({ width: w, height: h })}
    {@const left=Math.min(w*0.24,Math.max(64,...rowAxis.map(r=>r.label.length*7+12)))}
    {@const cw=(w-left-8)/Math.max(1,columnAxis.length)} {@const ch=(h-54)/Math.max(1,rowAxis.length)}
    <Canvas width={w} height={h} {label}>
      {#each columnAxis as column,i}<text x={left+(i+0.5)*cw} y={18} text-anchor="middle" fill={mutedInk} class="font-mono text-xs">{short(column.label,Math.max(4,Math.floor(cw/7)))}</text>{/each}
      {#each rowAxis as row,i}
        <text x={left-12} y={32+(i+0.5)*ch} text-anchor="end" dominant-baseline="middle" fill={mutedInk} class="font-mono text-xs">{short(row.label,24)}</text>
        {#each columnAxis as column,j}
          {@const cell=cells.find(c=>c.row===row.id&&c.column===column.id)}
          {@const intensity=cell?.value==null?0:Math.max(0,Math.min(1,project(cell.value,scale,0,1)))}
          <Mark id={cell?.id??JSON.stringify([row.id,column.id])} label={`${row.label} / ${column.label}: ${cell?.value==null?"Unavailable":format(cell.value)}`} {selected} onselect={interactive&&cell?select:undefined}>
            <rect x={left+j*cw+2} y={32+i*ch+2} width={Math.max(0,cw-4)} height={Math.max(0,ch-4)} fill={cell?.value==null?"none":tint(seriesColor(cell.series),8+intensity*48)} stroke={selected===cell?.id?ink:edge} stroke-dasharray={cell?.value==null?"3 3":undefined}/>
            <text x={left+(j+0.5)*cw} y={32+(i+0.5)*ch} text-anchor="middle" dominant-baseline="middle" fill={ink} class="font-mono text-xs">{cell?.value==null?"—":format(cell.value)}</text>
          </Mark>
        {/each}
      {/each}
      <text x={left} y={h-4} fill={mutedInk} class="text-xs">{format(scale[0])} → {format(scale[1])} · intensity</text>
    </Canvas>
  {/snippet}
</Frame>
