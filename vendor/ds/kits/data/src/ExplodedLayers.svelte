<script module lang="ts">
  export type { VisualizationSize, VisualizationSeries } from "./visualization";
  export type { ExplodedLayer } from "./exploded-layers";
</script>
<script lang="ts">
  import type { ExplodedLayersContract } from "./core/contracts/explodedlayers";
  import Frame from "./internal/VisualizationFrame.svelte";
  import Mark from "./internal/VisualizationMark.svelte";
  import { ink, mutedInk, seriesColor, tint, type VisualizationSize } from "./visualization";
  import { explodedLayers, explodedLayout, layerLines, type ExplodedLayer } from "./exploded-layers";
  interface Props extends ExplodedLayersContract {
    /** Reports selection for application-owned filtering or detail navigation. */
    onselectedchange?: (id: string) => void;
    /** Extra classes on the outer figure. */
    class?: string;
  }
  let { data, label = "ExplodedLayers", description, width, height, size = "lg", selected = $bindable(""), onselectedchange, interactive = true, detailHref, class: className = "" }: Props = $props();
  function select(id: string) { selected = id; onselectedchange?.(id); }
  const layers=$derived(explodedLayers(data));
  const rows=$derived(layers.map(l=>({...l,value:l.description??""})));
</script>
<Frame kind="ExplodedLayers" {label} {description} {width} {height} {size} {selected} {detailHref} rows={rows} onselect={interactive ? select : undefined} class={className} >
  {#snippet children({ width: w, height: h, unit })}
    {@const scene = explodedLayout(layers, w, h, unit)}
    <svg xmlns="http://www.w3.org/2000/svg" width={w} height={h} viewBox={`0 0 ${w} ${h}`} role="group" aria-label={label}>
      <title>{label}</title>
      {#if scene.length > 1}
        {@const first = scene[0]!} {@const last = scene[scene.length - 1]!}
        <g aria-hidden="true" stroke={tint(ink, 18)} stroke-dasharray="4 5">
          <line x1={first.left} x2={last.left} y1={first.cy} y2={last.cy}/>
          <line x1={first.right} x2={last.right} y1={first.cy} y2={last.cy}/>
        </g>
      {/if}
      <!-- Paint from the bottom up so each upper plate occludes the one beneath it. -->
      {#each [...scene].reverse() as layer (layer.id)}
        {@const color = seriesColor(layer.series)}
        {@const active = selected === layer.id}
        {@const titleLines = layerLines(layer.label, Math.max(8, Math.floor((layer.labelWidth - 28) / 7.5)))}
        {@const descriptionLines = layerLines(layer.description ?? "", Math.max(10, Math.floor((layer.labelWidth - 28) / 6.5)))}
        <Mark id={layer.id} label={`${layer.label}: ${layer.description ?? ""}`} {selected} onselect={interactive ? select : undefined}>
          <path data-layer-face="left" d={layer.leftFace} fill={tint(color, active ? 30 : 26)}/>
          <path data-layer-face="right" d={layer.rightFace} fill={tint(color, active ? 40 : 36)}/>
          <path data-layer-face="front" d={layer.front} fill="none" stroke={tint(color, 55)} stroke-linejoin="round"/>
          <path data-layer-face="top" d={layer.top} fill={tint(color, active ? 14 : 6)} stroke={active ? ink : tint(color, 55)} stroke-width={active ? 2 : 1.25}/>
          <g data-layer-callout>
            <line x1={layer.right + 8} x2={layer.labelX - 16} y1={layer.cy + layer.thickness / 2} y2={layer.cy + layer.thickness / 2} stroke={tint(ink, 24)}/>
            <circle cx={layer.right} cy={layer.cy + layer.thickness / 2} r={2.5} fill={mutedInk}/>
            <text aria-hidden="true" x={layer.labelX} y={layer.cy - 2} fill={mutedInk} class="font-mono text-xs">{String(layer.index + 1).padStart(2, "0")}</text>
            <text x={layer.labelX + 28} y={layer.cy - 2} fill={ink} class="text-sm font-medium">
              {#each titleLines as line, index}<tspan x={layer.labelX + 28} dy={index ? 18 : 0}>{line}</tspan>{/each}
            </text>
            {#if descriptionLines.length}
              <text x={layer.labelX + 28} y={layer.cy + 20 + (titleLines.length - 1) * 18} fill={mutedInk} class="text-xs">
                {#each descriptionLines as line, index}<tspan x={layer.labelX + 28} dy={index ? 17 : 0}>{line}</tspan>{/each}
              </text>
            {/if}
          </g>
        </Mark>
      {/each}
    </svg>
  {/snippet}
</Frame>
