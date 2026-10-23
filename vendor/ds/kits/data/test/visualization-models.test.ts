import { describe, expect, it } from "vitest";
import { waterfallSteps } from "../src/waterfall";
import { sankeyData } from "../src/sankey";
import { intervalRows } from "../src/intervals";
import { matrixData } from "../src/matrix-heatmap";
import { radarData } from "../src/radar";
import { schemaLayout } from "../src/schema-diagram";
import { diagramLayout, diagramDimensions } from "../src/diagram";
import { regression } from "../src/scatter";
import { treemapNodes } from "../src/treemap";
import { explodedLayers, explodedLayout, layerLines } from "../src/exploded-layers";
import * as f from "./fixtures/visualization-data";
describe("visualization measurement semantics",()=>{
 it("computes signed waterfall contributions, computed subtotals and total resets",()=>{
  const values=waterfallSteps([...f.costChanges,{id:"reset",label:"Reset",kind:"total",value:-10},{id:"next",label:"Next",value:3}]);
  expect(values[5]!.end).toBe(236);expect(values[6]!.end).toBe(-10);expect(values[7]!.end).toBe(-7);
 });
 it("rejects unknown Sankey endpoints, negative weights and loops without mutating data",()=>{
  const snapshot=JSON.stringify(f.requestFlow);const copy=sankeyData(f.requestFlow);copy.links[0]!.value=1;expect(JSON.stringify(f.requestFlow)).toBe(snapshot);
  expect(()=>sankeyData({...f.requestFlow,links:[{id:"x",label:"X",source:"gateway",target:"missing",value:2}]})).toThrow(/unknown/);
  expect(()=>sankeyData({...f.requestFlow,links:[{id:"x",label:"X",source:"gateway",target:"query",value:-1}]})).toThrow(/non-negative/);
  expect(()=>sankeyData({...f.requestFlow,links:[...f.requestFlow.links,{id:"back",label:"Back",source:"ok",target:"gateway",value:1}]})).toThrow(/acyclic/);
 });
 it("collapses descendants without losing intervals and detects parent cycles",()=>{
  expect(intervalRows(f.requestSpans,["execute"]).map(r=>r.id)).toEqual(["request","auth","plan","execute","response"]);
  expect(intervalRows([...f.requestSpans].reverse()).map(r=>r.id)[0]).toBe("request");
  expect(()=>intervalRows([{id:"a",label:"A",start:0,end:1,parentId:"b"},{id:"b",label:"B",start:0,end:1,parentId:"a"}])).toThrow(/Cyclic/);
  expect(()=>intervalRows([{id:"a",label:"A",start:2,end:1}])).toThrow(/precedes/);
 });
 it("keeps absent cells separate from observed zero and rejects duplicate coordinates",()=>{
  const cells=matrixData(f.matrixRows,f.matrixColumns,f.failureMatrix);expect(cells.some(c=>c.value===0)).toBe(true);expect(cells.some(c=>c.value===null)).toBe(true);
  expect(()=>matrixData(f.matrixRows,f.matrixColumns,[...f.failureMatrix,{...f.failureMatrix[0]!,id:"other"}])).toThrow(/Duplicate/);
 });
 it("distinguishes comparison series without requiring a caller-owned palette",()=>{
  const source=[{id:"a",label:"A",values:{latency:50}},{id:"b",label:"B",values:{latency:60}}];
  const radar=radarData(f.radarAxes,source);expect(radar[0]!.series).not.toBe(radar[1]!.series);expect(source[0]).not.toHaveProperty("series");
  const flow=sankeyData({nodes:[{id:"a",label:"A"},{id:"b",label:"B"}],links:[{id:"ab",label:"AB",source:"a",target:"b",value:1}]});
  expect(flow.nodes[0]!.series).not.toBe(flow.nodes[1]!.series);
 });
 it("does not fabricate radar measurements or silently clamp invalid scores",()=>{
  const series=[{id:"x",label:"X",values:{latency:50,throughput:null}}];expect(radarData(f.radarAxes,series)[0]!.values.throughput).toBeNull();
  expect(()=>radarData(f.radarAxes,[{id:"x",label:"X",values:{latency:101}}])).toThrow(/outside/);
 });
 it("validates field-level references and gives every schema card finite positions",()=>{
  const scene=schemaLayout(f.databaseSchema,800);expect(scene.nodes.every(n=>Number.isFinite(n.x+n.y+n.height))).toBe(true);
  expect(()=>schemaLayout({...f.databaseSchema,relations:[{...f.databaseSchema.relations[0]!,targetField:"absent"}]},800)).toThrow(/field/);
 });
 it("lays out cyclic architecture references deterministically without hanging",()=>{
  const data={nodes:[{id:"a",label:"A"},{id:"b",label:"B"}],edges:[{id:"ab",label:"AB",source:"a",target:"b"},{id:"ba",label:"BA",source:"b",target:"a"}]};
  expect(diagramLayout(data,800,400)).toEqual(diagramLayout(data,800,400));
 });
 it("fits exploded plates into short and tall canvases, including a single layer",()=>{
  for(const count of [0,1,3,6]) for(const [width,height] of [[480,192],[800,420],[1200,560]]) {
   const source=Array.from({length:count},(_,i)=>({id:String(i),label:`Layer ${i}`}));
   const scene=explodedLayout(explodedLayers(source),width!,height!);
   for(const plate of scene) {
    expect(plate.cy-plate.depth).toBeGreaterThanOrEqual(0);
    expect(plate.cy+plate.depth+plate.thickness).toBeLessThanOrEqual(height!);
    expect(plate.left).toBeGreaterThan(0);expect(plate.right).toBeLessThan(plate.labelX);
   }
   expect(source.every(layer=>!("series" in layer))).toBe(true);
   if(count>1)expect(scene[0]!.series).not.toBe(scene[1]!.series);
  }
 });
 it("wraps callouts at words and splits long identifiers without dropping content",()=>{
  expect(layerLines("Requests · spans · metrics",18)).toEqual(["Requests · spans ·","metrics"]);
  const identifier="a_very_long_unbroken_service_identifier";
  const lines=layerLines(identifier,12);expect(lines.join("")).toBe(identifier);
  expect(lines.every(line=>line.length<=12)).toBe(true);
 });
 it("keeps diagram spacing bounded in wide containers and separates cyclic nodes",()=>{
  for(const direction of ["right","down"] as const) {
   const dimensions=diagramDimensions(f.architecture,direction);
   const width=dimensions.widthUnits*32,height=dimensions.heightUnits*32;
   const scene=diagramLayout(f.architecture,width,height,direction);
   const roomy=diagramLayout(f.architecture,width+1200,height+600,direction);
   for(let i=1;i<scene.nodes.length;i++) {
    expect(roomy.nodes[i]!.x-roomy.nodes[0]!.x).toBeCloseTo(scene.nodes[i]!.x-scene.nodes[0]!.x);
    expect(roomy.nodes[i]!.y-roomy.nodes[0]!.y).toBeCloseTo(scene.nodes[i]!.y-scene.nodes[0]!.y);
   }
   for(const node of scene.nodes) {
    expect(node.x-node.width/2).toBeGreaterThanOrEqual(0);expect(node.x+node.width/2).toBeLessThanOrEqual(width);
    expect(node.y-node.height/2).toBeGreaterThanOrEqual(0);expect(node.y+node.height/2).toBeLessThanOrEqual(height);
   }
   const cycle=diagramLayout({nodes:[{id:"a",label:"A"},{id:"b",label:"B"}],edges:[{id:"ab",label:"AB",source:"a",target:"b"},{id:"ba",label:"BA",source:"b",target:"a"}]},800,400,direction);
   expect(cycle.nodes[0]![direction==="right"?"x":"y"]).not.toBe(cycle.nodes[1]![direction==="right"?"x":"y"]);
  }
 });
 it("omits undefined regression for a single x coordinate",()=>{expect(regression([{id:"a",label:"A",x:1,y:2},{id:"b",label:"B",x:1,y:3}])).toBeUndefined();});
 it("rejects duplicate treemap ids before building a hierarchy",()=>{expect(()=>treemapNodes({id:"a",label:"A",children:[{id:"a",label:"Other",value:3}]})).toThrow(/duplicate/);});
});
