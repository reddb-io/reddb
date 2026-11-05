import { finite, unique, type VisualizationItem, type VisualizationSeries } from "./visualization";
export interface RadarAxis { id: string; label: string; min?: number; max?: number; higherIsBetter?: boolean }
export interface RadarSeries extends VisualizationItem { values: Readonly<Record<string, number | null>> }
export function radarData(axes: readonly RadarAxis[], data: readonly RadarSeries[]) {
  unique(axes, "Radar axes"); unique(data, "Radar series");
  if (axes.length < 3) throw new Error("Radar requires at least three axes");
  for (const axis of axes) {
    const low=axis.min??0, high=axis.max??100;
    finite(low,axis.id); finite(high,axis.id);
    if(low>=high) throw new Error("Radar axis bounds must increase");
    for(const s of data) { const v=s.values[axis.id]; if(v!=null && (finite(v,axis.id)<low || v>high)) throw new Error(`Radar value outside ${axis.id} domain`); }
  }
  return data.map((item,index)=>({...item,series:item.series??((index%6+1) as VisualizationSeries)}));
}
export function radarPoint(index:number, count:number, fraction:number, cx:number, cy:number, radius:number) {
  const angle=index/count*Math.PI*2-Math.PI/2;
  return {x:cx+Math.cos(angle)*radius*fraction,y:cy+Math.sin(angle)*radius*fraction};
}
export function radarFraction(value:number,axis:RadarAxis) { const f=(value-(axis.min??0))/((axis.max??100)-(axis.min??0)); return axis.higherIsBetter===false?1-f:f; }
