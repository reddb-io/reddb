<script lang="ts">
  import type { SchemaPlotContract } from "../core/contracts/schemaplot";
  import Frame from "./VisualizationFrame.svelte";
  import Mark from "./VisualizationMark.svelte";
  import { schemaLayout, type SchemaData } from "../schema-diagram";
  import { ink, mutedInk, edge, surface, seriesColor, tint, short, type VisualizationSize } from "../visualization";
  interface Props extends SchemaPlotContract {onselectedchange?:(id:string)=>void;class?:string; }
  let {data,kind,label,description,width,height,size,selected=$bindable(""),onselectedchange,interactive=true,detailHref,class:className="",fieldLimit=8}:Props=$props();
  const rows=$derived([
    ...data.entities.map(e=>({...e,value:e.fields.map(f=>`${f.label}: ${f.type}${f.primaryKey?" PK":""}${f.foreignKey?" FK":""}${f.unique?" UQ":""}${f.nullable===false?" NN":""}`).join(" · ")})),
    ...data.relations.map(r=>({...r,value:`${r.source}${r.sourceField?"."+r.sourceField:""} → ${r.target}${r.targetField?"."+r.targetField:""}`,description:[r.sourceCardinality,r.targetCardinality,r.onDelete].filter(Boolean).join(" · ")}))
  ]);
  function select(id:string){selected=id;onselectedchange?.(id);}
  function layout(w:number,unit=32){if(!Number.isInteger(fieldLimit)||fieldLimit<1)throw new Error("fieldLimit must be a positive integer");return schemaLayout(data,w,fieldLimit,unit);}
</script>
<Frame {kind} {label} {description} {width} {height} {size} {selected} {detailHref} {rows} onselect={interactive?select:undefined} class={className}>
  {#snippet children({width:w,height:h,unit})}
    {@const scene=layout(w,unit)}
    <svg xmlns="http://www.w3.org/2000/svg" width={w} height={Math.max(h,scene.height)} viewBox={`0 0 ${w} ${Math.max(h,scene.height)}`} role="group" aria-label={label}>
      <title>{label}</title>
      {#each [...new Set(scene.nodes.flatMap(n=>n.group?[n.group]:[]))] as group}
        {@const members=scene.nodes.filter(n=>n.group===group)}
        {@const x=Math.min(...members.map(n=>n.x))-10} {@const y=Math.min(...members.map(n=>n.y))-12}
        {@const right=Math.max(...members.map(n=>n.x+n.width))+10} {@const bottom=Math.max(...members.map(n=>n.y+n.height))+12}
        <rect {x} {y} width={right-x} height={bottom-y} rx={8} fill="none" stroke={edge} stroke-dasharray="5 5"/>
        <text x={x+10} y={y-5} fill={mutedInk} class="font-mono text-xs">{group}</text>
      {/each}
      {#each data.relations as relation (relation.id)}
        {@const source=scene.nodes.find(n=>n.id===relation.source)!} {@const target=scene.nodes.find(n=>n.id===relation.target)!}
        {@const si=source.visible.findIndex(f=>f.id===relation.sourceField)} {@const ti=target.visible.findIndex(f=>f.id===relation.targetField)}
        {@const sameColumn=Math.abs(source.x-target.x)<1} {@const below=target.y>source.y+source.height} {@const forward=target.x>=source.x+source.width}
        {@const sx=below?source.x+source.width/2:forward?source.x+source.width:source.x}
        {@const tx=below?target.x+target.width/2:forward?target.x:target.x+target.width}
        {@const sy=below?source.y+source.height:source.y+(si>=0?source.header+source.inset+source.rowHeight*.4+si*source.rowHeight:26)}
        {@const ty=below?target.y:target.y+(ti>=0?target.header+target.inset+target.rowHeight*.4+ti*target.rowHeight:26)}
        {@const mid=below?(sy+ty)/2:(sx+tx)/2}
        {@const color=selected===relation.id?seriesColor(relation.series):mutedInk}
        {@const corridor=source.x+source.width+18}
        {@const fromY=source.y+(si>=0?source.header+source.inset+source.rowHeight*.4+si*source.rowHeight:source.header*.6)}
        {@const toY=target.y+(ti>=0?target.header+target.inset+target.rowHeight*.4+ti*target.rowHeight:target.header*.6)}
        <Mark id={relation.id} label={`${relation.label}: ${relation.source} → ${relation.target}${relation.onDelete?`, ON DELETE ${relation.onDelete}`:""}`} {selected} onselect={interactive?select:undefined}>
          <path d={sameColumn?`M${source.x+source.width},${fromY} H${corridor} V${toY} H${target.x+target.width}`:below?`M${sx},${sy} V${mid} H${tx} V${ty}`:`M${sx},${sy} H${mid} V${ty} H${tx}`} fill="none" stroke={color} stroke-width={selected===relation.id?2.5:1.5}/>
          <circle cx={tx} cy={ty} r={3} fill={color}/>
          <text x={below?sx+6:sx+(forward?5:-5)} y={sy-7} text-anchor={below||forward?"start":"end"} fill={color} class="font-mono text-xs">{relation.sourceCardinality??""}</text>
          <text x={below?tx+6:tx+(forward?-5:5)} y={ty-7} text-anchor={below||!forward?"start":"end"} fill={color} class="font-mono text-xs">{relation.targetCardinality??""}</text>
          {#if sameColumn}<text x={source.x+source.width/2} y={below?(source.y+source.height+target.y)/2:(target.y+target.height+source.y)/2} text-anchor="middle" fill={color} class="font-mono text-xs">{short(relation.label||relation.onDelete||"",32)}</text>{:else if below}<text x={(sx+tx)/2} y={mid-7} text-anchor="middle" fill={color} class="font-mono text-xs">{short(relation.label||relation.onDelete||"",28)}</text>{/if}
        </Mark>
      {/each}
      {#each scene.nodes as node (node.id)}
        {@const color=seriesColor(node.series)} {@const active=selected===node.id||node.kind==="aggregate"}
        <Mark id={node.id} label={`${node.label}: ${node.fields.length} fields`} {selected} onselect={interactive?select:undefined}>
          <rect x={node.x} y={node.y} width={node.width} height={node.height} rx={6} fill={surface} stroke={active?color:edge} stroke-width={active?2:1} stroke-dasharray={node.kind==="join"?"5 4":undefined}/>
          <rect x={node.x+1} y={node.y+1} width={node.width-2} height={node.header} rx={5} fill={active?tint(color,12):tint(mutedInk,5)}/>
          <text x={node.x+12} y={node.y+node.header*.62} fill={ink} class="text-sm font-medium">{short(node.label,Math.floor((node.width-70)/8))}</text>
          <text x={node.x+node.width-10} y={node.y+node.header*.62} text-anchor="end" fill={mutedInk} class="font-mono text-xs">{node.kind??(kind==="EntityRelationship"?"entity":"table")}</text>
          {#each node.visible as field,i}
            {@const y=node.y+node.header+node.inset+node.rowHeight*.4+i*node.rowHeight}
            <text x={node.x+12} {y} fill={ink} class="font-mono text-xs">{field.primaryKey?"# ":field.foreignKey?"→ ":""}{short(field.label,18)}</text>
            <text x={node.x+node.width-10} {y} text-anchor="end" fill={mutedInk} class="font-mono text-xs">{short(field.type,17)}{field.unique?" UQ":""}{field.nullable===false?" NN":""}</text>
          {/each}
          {#if node.fields.length>fieldLimit}<text x={node.x+12} y={node.y+node.header+node.inset+node.rowHeight*.4+fieldLimit*node.rowHeight} fill={mutedInk} class="text-xs">+ {node.fields.length-fieldLimit} more fields · Explore data</text>{/if}
          {#if node.indexes?.length}
            {@const y=node.y+node.header+node.inset+(node.visible.length+(node.fields.length>fieldLimit?1:0))*node.rowHeight}
            <line x1={node.x} x2={node.x+node.width} y1={y} y2={y} stroke={edge}/>
            <text x={node.x+12} y={y+20} fill={mutedInk} class="font-mono text-xs">INDEXES</text>
            {#each node.indexes as index,i}<text x={node.x+12} y={y+40+i*20} fill={mutedInk} class="font-mono text-xs">{short(index,Math.floor(node.width/7)-4)}</text>{/each}
          {/if}
        </Mark>
      {/each}
    </svg>
  {/snippet}
</Frame>
