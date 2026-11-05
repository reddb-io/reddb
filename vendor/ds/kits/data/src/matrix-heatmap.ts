import { finite, unique, type VisualizationItem } from "./visualization";
export interface MatrixAxis { id:string; label:string }
export interface MatrixCell extends VisualizationItem { row:string; column:string; value:number|null }
export function matrixData(rows:readonly MatrixAxis[],columns:readonly MatrixAxis[],data:readonly MatrixCell[]) {
  unique(rows,"Matrix rows");unique(columns,"Matrix columns");unique(data,"Matrix cells");
  const rowIds=new Set(rows.map(r=>r.id)),colIds=new Set(columns.map(c=>c.id)),pairs=new Set<string>();
  for(const cell of data) {
    if(!rowIds.has(cell.row)||!colIds.has(cell.column)) throw new Error("Matrix cell references an unknown axis");
    const pair=JSON.stringify([cell.row,cell.column]);if(pairs.has(pair))throw new Error("Duplicate matrix coordinate");pairs.add(pair);
    if(cell.value!==null)finite(cell.value,cell.id);
  }
  return data;
}
