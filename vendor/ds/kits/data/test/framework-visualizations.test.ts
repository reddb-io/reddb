import { scheduleOverlays, clippedWindow } from "../src/core/schedule-overlays";
import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h, type ComponentType } from "react";
import { createRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import * as react from "../react/src";
import Fixture from "./fixtures/FrameworkVisualizations.svelte";
Object.assign(globalThis,{IS_REACT_ACT_ENVIRONMENT:true});
const cleanups:(()=>Promise<void>)[]=[];afterEach(async()=>{while(cleanups.length)await cleanups.pop()!();});
const schema={entities:[{id:"users",label:"Users",fields:[{id:"id",label:"id",type:"uuid",primaryKey:true}]},{id:"requests",label:"Requests",fields:[{id:"owner",label:"owner_id",type:"uuid",foreignKey:true}]}],relations:[{id:"owns",label:"owns",source:"users",target:"requests",sourceField:"id",targetField:"owner",sourceCardinality:"1",targetCardinality:"n"}]};
const intervals=[{id:"request",label:"GET /records",start:0,end:100,progress:0.6},{id:"query",label:"Query",start:10,end:90,parentId:"request",tone:"danger",attributes:{statement:"SELECT * FROM records"}}];
const points=[{id:"api",label:"API",x:20,y:40,series:1},{id:"query",label:"Query",x:80,y:70,series:2}];
const defaults={
 Diagram:{data:{nodes:[{id:"ingress",label:"Ingress"},{id:"query",label:"Query",kind:"store"}],edges:[{id:"request",label:"Request",source:"ingress",target:"query"}]}},
 ExplodedLayers:{data:[{id:"surface",label:"Surface"},{id:"storage",label:"Storage"}]},
 SequenceDiagram:{data:{participants:[{id:"gateway",label:"Gateway"},{id:"query",label:"Query"}],messages:[{id:"request",label:"Request",source:"gateway",target:"query"}]}},
 MatrixHeatmap:{rowAxis:[{id:"api",label:"API"}],columnAxis:[{id:"sao",label:"São Paulo"},{id:"fra",label:"Frankfurt"}],data:[{id:"zero",label:"Idle",row:"api",column:"sao",value:0},{id:"missing",label:"Unavailable",row:"api",column:"fra",value:null}]},
 Waterfall:{data:[{id:"initial",label:"Initial",value:100,kind:"total"},{id:"compute",label:"Compute",value:42},{id:"credit",label:"Credit",value:-12},{id:"balance",label:"Balance",kind:"subtotal"}]},
 Scatter:{data:points},Quadrant:{data:points},
 Sankey:{data:{nodes:[{id:"in",label:"Ingress"},{id:"ok",label:"Success"},{id:"error",label:"Error"}],links:[{id:"success",label:"Successful requests",source:"in",target:"ok",value:80},{id:"failure",label:"Failed requests",source:"in",target:"error",value:20}]}},
 Radar:{axes:[{id:"latency",label:"Latency"},{id:"cost",label:"Cost"},{id:"throughput",label:"Throughput"}],data:[{id:"sao",label:"São Paulo",values:{latency:80,cost:40,throughput:90}}]},
 SchemaDiagram:{data:schema},EntityRelationship:{data:schema},Gantt:{data:intervals},TraceWaterfall:{data:intervals},
 BreakdownBar:{label:"Request outcomes",items:[{id:"ok",label:"Success",value:80},{id:"error",label:"Errors",value:20},{id:"zero",label:"No samples",value:0}]},
 KpiCard:{label:"Requests",value:"1,240",delta:-4.2,polarity:"lower-is-better",description:"Last hour"},
 HexViewer:{value:[0,65,255]},Diff:{before:{status:200,region:"sao"},after:{status:500,region:"sao"}},
 Treemap:{data:{id:"root",label:"Resources",children:[{id:"api",label:"API",value:80},{id:"query",label:"Query",value:20}]}},
};
type Name=keyof typeof defaults;type Flavor="svelte"|"react";
const props=(name:Name,extra:Record<string,any>={})=>({...defaults[name],width:640,height:320,...extra});
async function interact(fn:()=>void){await act(async()=>{fn();flushSync();});}
async function render(name:Name,flavor:Flavor,extra:Record<string,any>={}){
 const target=document.createElement("div");document.body.append(target);
 const native=(extra:Record<string,any>)=>Object.fromEntries(Object.entries(props(name,extra)).map(([key,value])=>[key==="className"?"class":key.startsWith("on")?key.toLowerCase():key,value]));
 if(flavor==="svelte"){const instance=mount(Fixture,{target,props:{name,options:native(extra)}});flushSync();cleanups.push(async()=>{await unmount(instance);target.remove();});return{target,update:async(extra:Record<string,any>)=>{flushSync(()=>instance.update(native(extra)));}};}
 const root=createRoot(target),element=(extra:Record<string,any>)=>h(react[name] as ComponentType<any>,props(name,extra));await act(()=>root.render(element(extra)));cleanups.push(async()=>{await act(()=>root.unmount());target.remove();});return{target,update:async(extra:Record<string,any>)=>{await act(()=>root.render(element(extra)));}};
}
for(const name of Object.keys(defaults) as Name[])it(`${name} renders meaningful native SSR without assigning appearance axes`,()=>{
 const html=renderToString(h(react[name] as ComponentType<any>,props(name)));expect(html).not.toMatch(/data-(?:theme|color-scheme|density)=/);expect(html.length).toBeGreaterThan(200);
 if(!["BreakdownBar","KpiCard","HexViewer","Diff"].includes(name)){expect(html).toContain("<svg");expect(html).toContain("Explore data");}
});
for(const flavor of ["svelte","react"] as const)describe(`${flavor} analytical and diagram contracts`,()=>{
 for(const name of Object.keys(defaults) as Name[])it(`mounts ${name} with data and keeps caller sizing intact`,async()=>{
  const r=await render(name,flavor);expect(r.target.firstElementChild).toBeTruthy();if(!["BreakdownBar","KpiCard","HexViewer","Diff"].includes(name)){expect(r.target.querySelector("svg")).toBeTruthy();expect(r.target.querySelector("table caption")?.textContent).toContain("data");}
 });
 for(const name of ["Diagram","ExplodedLayers","SequenceDiagram","MatrixHeatmap","Waterfall","Scatter","Quadrant","Sankey","Radar","SchemaDiagram","EntityRelationship","Gantt","TraceWaterfall","Treemap"] as const)it(`${name} shares keyboard selection and removes activation in static mode`,async()=>{
  const changed=vi.fn(),r=await render(name,flavor,{onSelectedChange:changed}),mark=r.target.querySelector<SVGGElement>('[data-mark-id][role="button"]')!;expect(mark).toBeTruthy();
  await interact(()=>mark.dispatchEvent(new KeyboardEvent("keydown",{key:"Enter",bubbles:true,cancelable:true})));expect(changed).toHaveBeenCalledWith(mark.dataset.markId);expect(mark.getAttribute("aria-pressed")).toBe("true");
  await r.update({interactive:false,onSelectedChange:changed});expect(r.target.querySelectorAll('[data-mark-id][role="button"]')).toHaveLength(0);
 });
 it("keeps request-flow widths proportional and leaves caller telemetry unchanged",async()=>{
  const original=structuredClone(defaults.Sankey.data),r=await render("Sankey",flavor),a=r.target.querySelector('[data-mark-id="success"] path')!,b=r.target.querySelector('[data-mark-id="failure"] path')!;
  expect(Number(a.getAttribute("stroke-width"))/Number(b.getAttribute("stroke-width"))).toBeCloseTo(4,6);expect(defaults.Sankey.data).toEqual(original);
 });
 it("retains zero and missing matrix observations as distinct readings",async()=>{
  const r=await render("MatrixHeatmap",flavor);expect(r.target.querySelector('[data-mark-id="zero"] title')?.textContent).toContain("0");expect(r.target.querySelector('[data-mark-id="missing"] title')?.textContent).toMatch(/unavailable|missing|No data/i);
 });
 it("preserves exact breakdown proportions and keeps zero groups available for drill-down",async()=>{
  const changed=vi.fn(),r=await render("BreakdownBar",flavor,{onSelectedChange:changed}),segments=r.target.querySelectorAll<HTMLElement>("[data-breakdown-segment]");expect([...segments].map(s=>s.style.width)).toEqual(["80%","20%"]);
  const zero=r.target.querySelector<HTMLButtonElement>('[data-breakdown-group="zero"]')!;await interact(()=>zero.click());expect(changed).toHaveBeenCalledWith("zero");await interact(()=>zero.click());expect(changed).toHaveBeenLastCalledWith(null);
 });
 it("keeps KPI polarity independent of delta sign and preserves zero",async()=>{
  const r=await render("KpiCard",flavor);expect(r.target.querySelector("section")!.dataset.deltaSentiment).toBe("improvement");await r.update({value:0,delta:0});expect(r.target.querySelector("[data-kpi-value]")!.textContent?.trim()).toBe("0");expect(r.target.querySelector("section")!.dataset.deltaSentiment).toBe("neutral");
 });
 it("collapses trace children without changing observed interval lengths or caller-owned selection",async()=>{
  const changed=vi.fn(),r=await render("TraceWaterfall",flavor,{onCollapsedChange:changed});await interact(()=>[...r.target.querySelectorAll("button")].find(button=>button.textContent?.trim()==="Collapse GET /records")!.click());expect(r.target.querySelector('[data-mark-id="query"]')).toBeNull();expect(changed).toHaveBeenCalledWith(["request"]);
 });
 it("keeps textual byte and diff semantics usable without any engine",async()=>{
  const hex=await render("HexViewer",flavor);expect(hex.target.querySelector("[data-hex-bytes]")!.textContent).toContain("00 41 ff");await hex.update({value:[]});expect(hex.target.textContent).toContain("No bytes");
  const diff=await render("Diff",flavor);expect(diff.target.textContent).toContain("status");expect(diff.target.textContent).toContain("500");await diff.update({after:defaults.Diff.before});expect(diff.target.textContent).toContain("unchanged");
 });
});

for (const flavor of ["svelte", "react"] as const) it(`${flavor} keeps schedule references readable outside the viewport and clips unavailability`, async () => {
  const r = await render("Gantt", flavor, { timeDomain: [0, 100], markers: [{ id: "due", label: "Deadline", time: 50 }, { id: "later", label: "Later", time: 150 }], windows: [{ id: "freeze", label: "Unavailable", start: -20, end: 25 }], selected: "request", showDetails: false });
  expect(r.target.querySelector('[data-schedule-marker="due"]')).not.toBeNull();
  expect(r.target.querySelector('[data-schedule-marker="later"]')).toBeNull();
  expect(r.target.querySelector('[aria-label$="schedule references"]')?.textContent).toContain("Later");
  expect(Number(r.target.querySelector('[data-schedule-window="freeze"]')?.getAttribute("width"))).toBeGreaterThan(0);
  expect(r.target.querySelector('[aria-label="GET /records details"]')).toBeNull();
  await r.update({ windows: [{ id: "row", label: "Query freeze", start: 10, end: 25, intervalId: "query" }], collapsed: ["request"] });
  expect(r.target.querySelector('[data-schedule-window="row"]')).toBeNull();
});

it("validates schedule references without changing caller-owned intervals", () => {
  expect(() => scheduleOverlays([{ id: "a", label: "Bad", time: NaN }], [], [])).toThrow();
  expect(() => scheduleOverlays([], [{ id: "a", label: "Backwards", start: 2, end: 1 }], [])).toThrow();
  expect(() => scheduleOverlays([], [{ id: "a", label: "Unknown", start: 1, end: 2, intervalId: "missing" }], [])).toThrow();
  expect(clippedWindow(-10, 20, [0, 100])).toEqual([0, 20]);
  expect(clippedWindow(100, 120, [0, 100])).toBeNull();
});
