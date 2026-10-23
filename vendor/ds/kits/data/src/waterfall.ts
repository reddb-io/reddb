import { finite, unique, type VisualizationItem } from "./visualization";
export interface WaterfallStep extends VisualizationItem { value?:number; kind?:"delta"|"total"|"subtotal" }
/** Totals set the running balance; subtotals observe it; deltas change it. */
export function waterfallSteps(data:readonly WaterfallStep[]) {
  unique(data,"Waterfall"); let balance=0;
  return data.map(item=>{
    const kind=item.kind??"delta";
    if(kind==="subtotal")return {...item,start:0,end:balance,amount:balance,kind};
    const value=finite(item.value??0,item.id);
    if(kind==="total") {balance=value;return {...item,start:0,end:balance,amount:value,kind};}
    const start=balance;balance+=value;return {...item,start,end:balance,amount:value,kind};
  });
}
