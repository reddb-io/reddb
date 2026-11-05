/** @vitest-environment node */
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import Scatter from "../src/Scatter.svelte";
import Radar from "../src/Radar.svelte";
import Quadrant from "../src/Quadrant.svelte";
import MatrixHeatmap from "../src/MatrixHeatmap.svelte";
import Waterfall from "../src/Waterfall.svelte";
import Treemap from "../src/Treemap.svelte";
import Sankey from "../src/Sankey.svelte";
import Gantt from "../src/Gantt.svelte";
import TraceWaterfall from "../src/TraceWaterfall.svelte";
import SchemaDiagram from "../src/SchemaDiagram.svelte";
import EntityRelationship from "../src/EntityRelationship.svelte";
import ExplodedLayers from "../src/ExplodedLayers.svelte";
import Diagram from "../src/Diagram.svelte";
import SequenceDiagram from "../src/SequenceDiagram.svelte";
import { renderStaticVisualization } from "../src/static.server";
import * as f from "./fixtures/visualization-data";
const fixtures = [
 [Scatter,{data:f.requestSamples}], [Radar,{data:f.benchmarks,axes:f.radarAxes}],
 [Quadrant,{data:f.priorities}], [MatrixHeatmap,{data:f.failureMatrix,rowAxis:f.matrixRows,columnAxis:f.matrixColumns}],
 [Waterfall,{data:f.costChanges}], [Treemap,{data:f.costTree}], [Sankey,{data:f.requestFlow}],
 [Gantt,{data:f.schedule}], [TraceWaterfall,{data:f.requestSpans}],
 [SchemaDiagram,{data:f.databaseSchema}], [EntityRelationship,{data:f.domainModel}],
 [ExplodedLayers,{data:f.architectureLayers}], [Diagram,{data:f.architecture}], [SequenceDiagram,{data:f.requestSequence}],
] as const;
describe("visualization first paint",()=>{
 for(const [component,props] of fixtures) it(`renders ${component.name} as actual SVG, with exact accessible data`,()=>{
   const {body}=render(component as typeof Scatter,{props:{...props,label:"Example",width:800,height:450} as never});
   expect(body).toContain("<svg");expect(body).toContain('aria-label="Example"');expect(body).toContain("Explore data");
   expect(body).not.toMatch(/NaN|Infinity|undefined|data-chart-placeholder/);
   expect(body).toContain("<title>");
 });
 it("build-only output contains no script, handler, controls or hydration runtime",()=>{
   const html=renderStaticVisualization(Scatter,{data:f.requestSamples,label:"Static"});
   expect(html).toContain("<svg");expect(html).not.toMatch(/<script|onclick=|onkeydown=|role="button"|aria-pressed=/);
 });
});
