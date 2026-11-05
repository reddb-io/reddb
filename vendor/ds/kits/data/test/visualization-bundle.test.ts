/** @vitest-environment node */
import { gzipSync } from "node:zlib";
import { join } from "node:path";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { build, type Rolldown } from "vite";
import { describe, expect, it } from "vitest";
const root=join(import.meta.dirname,"..");
async function consumer(component:string){
 const result=await build({root,configFile:false,logLevel:"silent",plugins:[svelte()],resolve:{conditions:["browser"],alias: [ { find: /^@reddb-io\/design-system\/base\/recipes$/, replacement: join(root, "../base/src/recipes.ts") }, { find: /^@reddb-io\/design-system\/base$/, replacement: join(root, component.startsWith("react/") ? "../base/react/src/index.ts" : "../base/src/index.ts") } ]},build:{write:false,minify:true,lib:{entry:join(root,component),formats:["es"]},rolldownOptions:{external:[/^svelte(\/|$)/, /^react(\/|$)/, /^react-dom(\/|$)/]}}});
 const out=(Array.isArray(result)?result[0]:result) as Rolldown.RolldownOutput;
 const chunks=out.output.filter((c):c is Rolldown.OutputChunk=>c.type==="chunk");
 return {modules:chunks.flatMap(c=>c.moduleIds),bytes:chunks.reduce((s,c)=>s+gzipSync(c.code).length,0),code:chunks.map(c=>c.code).join("\n")};
}
describe("actual per-component production bundles",()=>{
 for(const name of ["VirtualList","PanZoomViewport","GeoMap","TerminalSession"]){
  it(`${name} has no optional rendering engine or bundled geographic dataset`,async()=>{
   const out=await consumer(`src/${name}.svelte`);
   expect(out.modules.filter(m=>/@xterm|terminal-adapter|layerchart|d3-|sigma|graphology|natural-earth|ne_110m|fixtures\/geography|static\.server/.test(m))).toEqual([]);
   expect(out.bytes).toBeLessThan(30_000);
   if(name!=="GeoMap") expect(out.modules.some(m=>/geo-map\.(ts|svelte)/.test(m))).toBe(false);
   console.info(`${name} consumer gzip: ${out.bytes} bytes`);
  },180000);
 }
 for(const flavor of ["svelte","react"]) it(`${flavor} SceneViewport adds labels and camera behavior without a rendering engine`,async()=>{
   const out=await consumer(flavor === "react" ? "react/src/SceneViewport.tsx" : "src/SceneViewport.svelte");
   expect(out.code).toContain("data-scene-annotation");
   expect(out.modules.filter(m=>/@xterm|layerchart|d3-|sigma|graphology|three|babylon|static\.server/.test(m))).toEqual([]);
   if(flavor === "react") expect(out.modules.some(m=>/\/svelte\/|\.svelte$/.test(m))).toBe(false);
   expect(out.bytes).toBeLessThan(30_000);
   console.info(`${flavor} SceneViewport consumer gzip: ${out.bytes} bytes`);
 },180000);
 it("keeps every visualization engine out of a control-only landing",async()=>{
  const out=await consumer("test/fixtures/one-component.ts");
  expect(out.modules.some(m=>/layerchart|d3-|sigma|graphology|kits\/data\/src/.test(m))).toBe(false);
 },180000);
 it("does not ship a chart engine, other diagrams or a static server renderer with SVG layers",async()=>{
  const out=await consumer("src/ExplodedLayers.svelte");
  expect(out.code).toContain("ExplodedLayers");
  expect(out.modules.some(m=>/layerchart|d3-|sigma|graphology|static\.server|schema-diagram|intervals\.ts/.test(m))).toBe(false);
  expect(out.bytes).toBeLessThan(22_000);
 },180000);
 it("includes only the requested chart family and excludes hierarchy, flow, graph and server engines",async()=>{
  const out=await consumer("src/Scatter.svelte");
  expect(out.modules.some(m=>m.includes("layerchart"))).toBe(true);
  const forbidden=out.modules.filter(m=>/d3-sankey|d3-hierarchy|@dagrejs|sigma|graphology|static\.server|\/src\/(radar|intervals|schema-diagram)\./.test(m));
  expect(forbidden).toEqual([]);
  console.info(`Scatter consumer gzip: ${out.bytes} bytes`);
  // Full LayerChart Core + DS controls, Svelte external. Measured 147 kB gzip; <10% headroom.
  expect(out.bytes).toBeLessThan(160_000);
 },180000);
});
