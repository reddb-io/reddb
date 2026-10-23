import { unique, finite, type VisualizationSeries } from "./visualization";

export interface GeoMapPoint {
  id: string;
  label: string;
  latitude: number;
  longitude: number;
  description?: string;
  series?: VisualizationSeries;
}
export interface GeoMapRoute { id: string; source: string; target: string; label: string; series?: VisualizationSeries }
export type GeoPosition = readonly number[];
export type GeoMapGeometry = { type: "Polygon"; coordinates: readonly (readonly GeoPosition[])[] }
  | { type: "MultiPolygon"; coordinates: readonly (readonly (readonly GeoPosition[])[])[] };
/** The subset of GeoJSON needed for a provider-independent land layer. */
export interface GeoMapGeography {
  type: "FeatureCollection";
  features: readonly { type: "Feature"; geometry: GeoMapGeometry | null; properties?: unknown }[];
}
export const MAP_WIDTH = 1000, MAP_HEIGHT = 500;
/** WGS84 equirectangular projection. This is not a distance or area calculation. */
export function geoPosition(longitude: number, latitude: number): [number, number] {
  finite(longitude,"GeoMap longitude"); finite(latitude,"GeoMap latitude");
  if (longitude < -180 || longitude > 180 || latitude < -90 || latitude > 90) throw new Error("GeoMap: WGS84 coordinates are out of range");
  return [(longitude+180)/360*MAP_WIDTH,(90-latitude)/180*MAP_HEIGHT];
}
export function geoLand(geography?: GeoMapGeography): string[] {
  if (!geography) return [];
  if (geography.type !== "FeatureCollection") throw new Error("GeoMap: expected a GeoJSON FeatureCollection");
  return geography.features.flatMap(feature => {
    const g = feature.geometry;
    if (!g) return [];
    if (g.type !== "Polygon" && g.type !== "MultiPolygon") throw new Error("GeoMap: land supports Polygon and MultiPolygon geometry");
    const polygons = g.type === "Polygon" ? [g.coordinates] : g.coordinates;
    return polygons.map(polygon => polygon.map(ring => {
      if (ring.length < 4) throw new Error("GeoMap: a polygon ring needs at least four positions");
      return ring.map((position,index) => {
        if (position.length < 2) throw new Error("GeoMap: incomplete GeoJSON position");
        const [x,y] = geoPosition(position[0]!,position[1]!); return `${index ? "L" : "M"}${x},${y}`;
      }).join(" ")+" Z";
    }).join(" "));
  });
}
export function geoData(points: readonly GeoMapPoint[], routes: readonly GeoMapRoute[]) {
  unique(points,"GeoMap points"); unique(routes,"GeoMap routes");
  const projected = points.map(point => ({...point,position:geoPosition(point.longitude,point.latitude)}));
  const byId = new Map(projected.map(point=>[point.id,point]));
  const paths = routes.map(route => {
    const a = byId.get(route.source), b = byId.get(route.target);
    if (!a || !b) throw new Error(`GeoMap: unknown endpoint in route ${route.id}`);
    const [x1,y1] = a.position; let [x2,y2] = b.position;
    if (x2-x1 > MAP_WIDTH/2) x2 -= MAP_WIDTH;
    if (x2-x1 < -MAP_WIDTH/2) x2 += MAP_WIDTH;
    const bend = Math.min(60,Math.hypot(x2-x1,y2-y1)*.15);
    const path = (shift:number) => `M${x1+shift},${y1} Q${(x1+x2)/2+shift},${(y1+y2)/2-bend} ${x2+shift},${y2}`;
    return {...route,paths:[path(0),...(x2<0?[path(MAP_WIDTH)]:x2>MAP_WIDTH?[path(-MAP_WIDTH)]:[])]};
  });
  return {points:projected,routes:paths};
}

/** Retain selected labels first, then omit collisions using measured type geometry. */
export function geoLabelIds(points: readonly (GeoMapPoint & {position:readonly number[]})[], selected: string, scale: number, glyphWidth: number, lineHeight: number): Set<string> {
  const boxes: {left:number;right:number;top:number;bottom:number}[]=[];
  const result=new Set<string>();
  const ordered=[...points.filter(p=>p.id===selected),...points.filter(p=>p.id!==selected)];
  for(const point of ordered){
    const x=point.position[0]!*scale,y=point.position[1]!*scale,w=point.label.length*glyphWidth;
    const left=point.position[0]!>800?x-12-w:x+12;
    const box={left,right:left+w,top:y-10-lineHeight,bottom:y-10};
    if(point.id!==selected&&boxes.some(b=>box.left<b.right+glyphWidth&&box.right+glyphWidth>b.left&&box.top<b.bottom+glyphWidth&&box.bottom+glyphWidth>b.top))continue;
    boxes.push(box);result.add(point.id);
  }
  return result;
}
