// Illustrative telemetry and models, never a claim about production observations.
import type { SankeyData } from "@reddb-io/design-system/data/Sankey";
import type { TimeInterval } from "@reddb-io/design-system/data/TraceWaterfall";
import type { MatrixCell } from "@reddb-io/design-system/data/MatrixHeatmap";
import type { QuadrantPoint } from "@reddb-io/design-system/data/Quadrant";
import type { RadarSeries } from "@reddb-io/design-system/data/Radar";
import type { ScatterPoint } from "@reddb-io/design-system/data/Scatter";
import type { TreemapNode } from "@reddb-io/design-system/data/Treemap";
import type { WaterfallStep } from "@reddb-io/design-system/data/Waterfall";
import type { SchemaData } from "@reddb-io/design-system/data/SchemaDiagram";
import type { ExplodedLayer } from "@reddb-io/design-system/data/ExplodedLayers";
import type { DiagramData } from "@reddb-io/design-system/data/Diagram";
import type { SequenceData } from "@reddb-io/design-system/data/SequenceDiagram";

export const requestFlow = {
 nodes:[{id:"gateway",label:"Gateway"},{id:"query",label:"Query",series:2},{id:"ingest",label:"Ingest",series:3},{id:"ok",label:"Success",series:2},{id:"error",label:"Error",series:4},{id:"timeout",label:"Timeout",series:5}],
 links:[{id:"gq",label:"Query requests",source:"gateway",target:"query",value:6500},{id:"gi",label:"Ingest requests",source:"gateway",target:"ingest",value:3500,series:3},
 {id:"qo",label:"Query successes",source:"query",target:"ok",value:6100,series:2},{id:"qe",label:"Query errors",source:"query",target:"error",value:250,series:4},{id:"qt",label:"Query timeouts",source:"query",target:"timeout",value:150,series:5},
 {id:"io",label:"Ingest successes",source:"ingest",target:"ok",value:3000,series:2},{id:"ie",label:"Ingest errors",source:"ingest",target:"error",value:350,series:4},{id:"it",label:"Ingest timeouts",source:"ingest",target:"timeout",value:150,series:5}]
} satisfies SankeyData;
export const requestSpans = [
 {id:"request",label:"POST /query",start:0,end:245,attributes:{"trace.id":"7fa2c9","http.status_code":"200","service.name":"gateway"}},
 {id:"auth",label:"Authorize",parentId:"request",start:4,end:22,series:2,attributes:{"cache.hit":"true"}},
 {id:"plan",label:"Plan query",parentId:"request",start:25,end:55,series:3},
 {id:"execute",label:"Execute query",parentId:"request",start:60,end:210,series:2},
 {id:"cache",label:"Cache lookup",parentId:"execute",start:60,end:72,series:3},
 {id:"attempt",label:"Database · attempt 1",parentId:"execute",start:65,end:95,tone:"danger",attributes:{"error.type":"connection_reset","retry.attempt":"1"}},
 {id:"db",label:"Database · attempt 2",parentId:"execute",start:105,end:190,series:4,attributes:{"db.operation.name":"SELECT","retry.attempt":"2"}},
 {id:"decode",label:"Decode rows",parentId:"execute",start:195,end:210,series:5},
 {id:"response",label:"Serialize response",parentId:"request",start:215,end:245,series:3}
] satisfies readonly TimeInterval[];
export const schedule=[
 {id:"rollout",label:"Monitor rollout",start:0,end:14,progress:.55},
 {id:"capture",label:"Trace collection",parentId:"rollout",start:0,end:5,progress:1,series:2},
 {id:"aggregate",label:"Flow aggregation",parentId:"rollout",start:3,end:8,progress:.7,series:3,dependencies:["capture"]},
 {id:"visuals",label:"Visualizations",parentId:"rollout",start:5,end:11,progress:.4,series:4,dependencies:["capture"]},
 {id:"validate",label:"Validation",parentId:"rollout",start:11,end:14,progress:0,series:5,dependencies:["visuals","aggregate"]},
 {id:"launch",label:"Release milestone",parentId:"rollout",start:14,end:14,progress:0,series:2,dependencies:["validate"]}
] satisfies readonly TimeInterval[];
export const matrixRows=[{id:"gateway",label:"API gateway"},{id:"query",label:"Query engine"},{id:"ingest",label:"Ingestion"},{id:"storage",label:"Storage"}];
export const matrixColumns=Array.from({length:6},(_,i)=>({id:`h${i}`,label:`${10+i}:00`}));
export const failureMatrix=matrixRows.flatMap((r,ri)=>matrixColumns.map((c,ci)=>({id:`${r.id}-${c.id}`,label:`${r.label} · ${c.label}`,row:r.id,column:c.id,value:ri===2&&ci===3?9.2:ri===3&&ci===5?null:(ri*3+ci)%5,series:2 as const}))) satisfies readonly MatrixCell[];
export const priorities=[{id:"retry",label:"Retry budget",x:22,y:85},{id:"cache",label:"Cache policy",x:38,y:65,series:2},{id:"index",label:"Index strategy",x:76,y:86,series:3},{id:"logs",label:"Log retention",x:20,y:24,series:4},{id:"rewrite",label:"Full rewrite",x:84,y:18,series:5}] satisfies readonly QuadrantPoint[];
export const radarAxes=[{id:"latency",label:"Latency score",max:100},{id:"throughput",label:"Throughput",max:100},{id:"reliability",label:"Reliability",max:100},{id:"cost",label:"Cost efficiency",max:100},{id:"operations",label:"Operational simplicity",max:100}];
export const benchmarks=[{id:"balanced",label:"Balanced",series:1,values:{latency:82,throughput:75,reliability:92,cost:78,operations:85}},{id:"throughput",label:"Throughput optimized",series:2,values:{latency:72,throughput:96,reliability:90,cost:60,operations:66}},{id:"economy",label:"Economy",series:3,values:{latency:58,throughput:64,reliability:85,cost:95,operations:78}}] satisfies readonly RadarSeries[];
export const requestSamples=Array.from({length:24},(_,i)=>({id:`req-${i}`,label:`Request ${i+1}`,x:4+i*7,y:20+i*5+(i%5)*9,series:(i%2+1) as 1|2})) satisfies readonly ScatterPoint[];
export const costTree={id:"all",label:"Compute",children:[{id:"query",label:"Query",series:1,children:[{id:"primary",label:"Primary",value:5200},{id:"replica",label:"Replicas",value:2100,series:2}]},{id:"ingest",label:"Ingestion",series:3,children:[{id:"workers",label:"Workers",value:1800,series:3},{id:"queue",label:"Queue",value:900,series:4}]},{id:"storage",label:"Storage",value:1600,series:5}]} satisfies TreemapNode;
export const costChanges=[{id:"base",label:"Baseline",kind:"total",value:240},{id:"traffic",label:"Traffic",value:64,series:2},{id:"reserved",label:"Commitments",value:-38,series:3},{id:"analytics",label:"Analytics",value:22,series:4},{id:"tiering",label:"Tiering",value:-52,series:5},{id:"total",label:"Current",kind:"subtotal"}] satisfies readonly WaterfallStep[];
const idField={id:"id",label:"id",type:"uuid",primaryKey:true,nullable:false};
export const databaseSchema={entities:[
 {id:"requests",label:"public.requests",fields:[idField,{id:"service_id",label:"service_id",type:"uuid",foreignKey:true,nullable:false},{id:"status",label:"status",type:"text"},{id:"duration",label:"duration_ms",type:"numeric"},{id:"created",label:"created_at",type:"timestamptz"}],indexes:["idx_requests_service_id","idx_requests_created_at"]},
 {id:"services",label:"public.services",fields:[idField,{id:"name",label:"name",type:"text",unique:true},{id:"region",label:"region",type:"text"}]},
 {id:"spans",label:"telemetry.spans",group:"Telemetry",fields:[idField,{id:"request_id",label:"request_id",type:"uuid",foreignKey:true},{id:"parent_id",label:"parent_id",type:"uuid",nullable:true},{id:"start",label:"start_time",type:"timestamptz"},{id:"end",label:"end_time",type:"timestamptz"}]}
],relations:[{id:"rs",label:"served by",source:"requests",target:"services",sourceField:"service_id",targetField:"id",sourceCardinality:"N",targetCardinality:"1",onDelete:"RESTRICT"},{id:"sr",label:"ON DELETE CASCADE",source:"spans",target:"requests",sourceField:"request_id",targetField:"id",sourceCardinality:"N",targetCardinality:"1",onDelete:"CASCADE",series:4}]} satisfies SchemaData;
export const domainModel={entities:[{id:"request",label:"Request",kind:"aggregate",fields:[idField,{id:"status",label:"status",type:"RequestStatus"}]},{id:"service",label:"Service",kind:"entity",fields:[idField,{id:"name",label:"name",type:"string"}]},{id:"tag",label:"Tag",kind:"entity",fields:[idField,{id:"label",label:"label",type:"string"}]},{id:"requestTag",label:"RequestTag",kind:"join",fields:[{id:"request_id",label:"request_id",type:"uuid",foreignKey:true},{id:"tag_id",label:"tag_id",type:"uuid",foreignKey:true}]}],relations:[{id:"serves",label:"handles",source:"service",target:"request",sourceCardinality:"1",targetCardinality:"N"},{id:"tagged",label:"tagged with",source:"request",target:"requestTag",sourceCardinality:"1",targetCardinality:"N"},{id:"tags",label:"classifies",source:"tag",target:"requestTag",sourceCardinality:"1",targetCardinality:"N"}]} satisfies SchemaData;
export const architectureLayers=[{id:"interface",label:"Interface",description:"Monitor · request explorer"},{id:"processing",label:"Processing",description:"Aggregation · traces · alerts",series:2},{id:"data",label:"Data",description:"Requests · spans · metrics",series:3}] satisfies readonly ExplodedLayer[];
export const architecture={nodes:[{id:"client",label:"Client",kind:"terminal"},{id:"gateway",label:"Gateway",description:"Auth + routing"},{id:"query",label:"Query engine"},{id:"storage",label:"Storage",kind:"store"},{id:"telemetry",label:"Telemetry",kind:"store",series:3}],edges:[{id:"cg",label:"request",source:"client",target:"gateway"},{id:"gq",label:"execute",source:"gateway",target:"query"},{id:"qs",label:"read / write",source:"query",target:"storage"},{id:"qt",label:"spans",source:"query",target:"telemetry",dashed:true}]} satisfies DiagramData;
export const requestSequence={participants:[{id:"client",label:"Client"},{id:"api",label:"Gateway"},{id:"engine",label:"Query engine"},{id:"db",label:"Database"}],messages:[{id:"a",label:"POST /query",source:"client",target:"api"},{id:"b",label:"Authorize + execute",source:"api",target:"engine"},{id:"c",label:"SELECT",source:"engine",target:"db"},{id:"d",label:"Rows",source:"db",target:"engine",kind:"return"},{id:"e",label:"Query result",source:"engine",target:"api",kind:"return"},{id:"f",label:"200 OK",source:"api",target:"client",kind:"return"}]} satisfies SequenceData;
