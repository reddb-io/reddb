<script module lang="ts">
  export type { VisualizationSize, VisualizationSeries } from "./visualization";
  export type { SequenceData, SequenceParticipant, SequenceMessage } from "./sequence-diagram";
</script>
<script lang="ts">
  import type { SequenceDiagramContract } from "./core/contracts/sequencediagram";
  import Frame from "./internal/VisualizationFrame.svelte";
  import Mark from "./internal/VisualizationMark.svelte";
  import { edge, ink, mutedInk, surface, seriesColor, tint, number, short, type VisualizationSize } from "./visualization";
  import { sequenceData, type SequenceData } from "./sequence-diagram";
  interface Props extends SequenceDiagramContract {
    /** Reports selection for application-owned filtering or detail navigation. */
    onselectedchange?: (id: string) => void;
    /** Extra classes on the outer figure. */
    class?: string;
  }
  let { data, label = "SequenceDiagram", description, width, height, size = "lg", selected = $bindable(""), onselectedchange, interactive = true, detailHref, class: className = "" }: Props = $props();
  function select(id: string) { selected = id; onselectedchange?.(id); }
  const sequence=$derived(sequenceData(data));
  const rows=$derived(sequence.messages.map(m=>({...m,value:`${m.source} → ${m.target}`,description:m.kind??"call"})));
</script>
<Frame kind="SequenceDiagram" {label} {description} {width} {height} {size} {selected} {detailHref} rows={rows} onselect={interactive ? select : undefined} class={className} >
  {#snippet children({ width: w, height: h })}
    {@const band=w/Math.max(1,sequence.participants.length)} {@const step=(h-90)/Math.max(1,sequence.messages.length)}
    <svg xmlns="http://www.w3.org/2000/svg" width={w} height={h} viewBox={`0 0 ${w} ${h}`} role="group" aria-label={label}>
      <title>{label}</title>
      {#each sequence.participants as p,i}
        {@const x=(i+.5)*band}
        <rect x={x-band*.4} y={8} width={band*.8} height={34} rx={4} fill={surface} stroke={edge}/>
        <text {x} y={30} text-anchor="middle" fill={ink} class="text-xs font-medium">{short(p.label,Math.floor(band/8))}</text>
        <line x1={x} x2={x} y1={42} y2={h-8} stroke={edge} stroke-dasharray="4 4"/>
      {/each}
      {#each sequence.messages as m,i (m.id)}
        {@const sx=(sequence.participants.findIndex(p=>p.id===m.source)+.5)*band} {@const tx=(sequence.participants.findIndex(p=>p.id===m.target)+.5)*band} {@const y=76+i*step} {@const sign=tx>=sx?1:-1}
        <Mark id={m.id} label={`${m.label}: ${m.source} → ${m.target}`} {selected} onselect={interactive?select:undefined}>
          {#if sx===tx}<path d={`M${sx},${y} h32 v16 h-32`} fill="none" stroke={seriesColor(m.series)}/>{:else}<line x1={sx} x2={tx} y1={y} y2={y} stroke={seriesColor(m.series)} stroke-width={selected===m.id?2.5:1.5} stroke-dasharray={m.kind==="return"?"5 4":undefined}/>{/if}
          <path d={`M${tx-sign*7},${y-4} L${tx},${y} L${tx-sign*7},${y+4}`} fill={m.kind==="async"?"none":seriesColor(m.series)} stroke={seriesColor(m.series)}/>
          <text x={(sx+tx)/2+(sx===tx?38:0)} y={y-9} text-anchor={sx===tx?"start":"middle"} fill={selected===m.id?ink:mutedInk} class="text-xs">{short(m.label,Math.max(12,Math.floor(Math.abs(tx-sx)/7)))}</text>
        </Mark>
      {/each}
    </svg>
  {/snippet}
</Frame>
