import { domain, finite, unique, type VisualizationItem } from "./visualization";
export interface ScatterPoint extends VisualizationItem { x: number; y: number; radius?: number }
export function scatterData(data: readonly ScatterPoint[]) {
  unique(data, "Scatter");
  for (const p of data) { finite(p.x, p.id); finite(p.y, p.id); if (p.radius !== undefined && (!Number.isFinite(p.radius) || p.radius < 0)) throw new Error("Point radius must be non-negative"); }
  return data;
}
/** Least-squares trend, absent for fewer than two distinct x values. */
export function regression(data: readonly ScatterPoint[]) {
  if (data.length < 2) return undefined;
  const n = data.length, x = data.reduce((s,p)=>s+p.x,0)/n, y=data.reduce((s,p)=>s+p.y,0)/n;
  const xx=data.reduce((s,p)=>s+(p.x-x)**2,0);
  if (!xx) return undefined;
  const slope=data.reduce((s,p)=>s+(p.x-x)*(p.y-y),0)/xx;
  const range=domain(data.map(p=>p.x));
  return range.map(v=>({x:v,y:y+slope*(v-x)}));
}
