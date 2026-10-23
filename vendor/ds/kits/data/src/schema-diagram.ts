import { unique, type VisualizationItem } from "./visualization";
export interface SchemaField { id:string; label:string; type:string; primaryKey?:boolean; foreignKey?:boolean; unique?:boolean; nullable?:boolean }
export interface SchemaEntity extends VisualizationItem { fields:readonly SchemaField[]; group?:string; kind?:"entity"|"table"|"join"|"aggregate"; indexes?:readonly string[]; x?:number; y?:number }
export interface SchemaRelation extends VisualizationItem { source:string; target:string; sourceField?:string; targetField?:string; sourceCardinality?:string; targetCardinality?:string; onDelete?:"CASCADE"|"RESTRICT"|"SET NULL"|"NO ACTION" }
export interface SchemaData { entities:readonly SchemaEntity[]; relations:readonly SchemaRelation[] }
export function schemaLayout(data:SchemaData, width:number, fieldLimit=8, unit=32) {
  unique(data.entities,"Schema entities");unique(data.relations,"Schema relations");
  const map=new Map(data.entities.map(e=>[e.id,e]));
  for(const e of data.entities) {unique(e.fields,`${e.id} fields`);if(e.x!==undefined&&!Number.isFinite(e.x)||e.y!==undefined&&!Number.isFinite(e.y))throw new Error("Invalid schema position");}
  for(const r of data.relations) {
    const source=map.get(r.source),target=map.get(r.target);
    if(!source||!target)throw new Error("Unknown relation entity");
    if(r.sourceField&&!source.fields.some(f=>f.id===r.sourceField)||r.targetField&&!target.fields.some(f=>f.id===r.targetField))throw new Error("Unknown relation field");
  }
  const header=Math.max(28,unit*1.375),rowHeight=Math.max(18,unit*.8125),inset=unit*.375;
  const columns=Math.max(1,Math.floor(width/340)), cardWidth=Math.max(250,(width-48)/columns-40);
  const rowHeights:number[]=[];
  const nodes=data.entities.map((entity,index)=>{
    const row=Math.floor(index/columns),visible=entity.fields.slice(0,fieldLimit);
    const height=header+inset+(visible.length+(entity.fields.length>fieldLimit?1:0))*rowHeight+(entity.indexes?.length?unit*1.125+entity.indexes.length*rowHeight*.77:0);
    rowHeights[row]=Math.max(rowHeights[row]??0,height);
    return {...entity,visible,width:cardWidth,height,row,index,header,rowHeight,inset};
  });
  const placed=nodes.map(n=>({...n,x:n.x??24+(n.index%columns)*(cardWidth+40),y:n.y??24+rowHeights.slice(0,n.row).reduce((s,v)=>s+v+unit*2.875,0)}));
  return {nodes:placed,height:Math.max(200,...placed.map(n=>n.y+n.height+24))};
}
