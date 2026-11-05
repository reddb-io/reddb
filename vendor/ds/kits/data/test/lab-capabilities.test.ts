import { describe,expect,it } from "vitest";
import { virtualRange } from "../src/virtual-list";
import { fitView,zoomView } from "../src/pan-zoom";
import { geoData,geoLand,geoPosition,geoLabelIds } from "../src/geo-map";

describe("bounded virtual lists",()=>{
  it("bounds the mounted range independently of the collection's size",()=>{
    const a=virtualRange(10_000,64,384,6400,2);expect(a).toEqual({start:98,end:108,top:6400});
    expect(virtualRange(10_000_000,64,384,6400,2).end-a.start).toBe(10);
  });
  it("clamps stale scroll positions after filtering and handles empty and short lists",()=>{
    expect(virtualRange(3,64,384,6400,2)).toEqual({start:0,end:3,top:0});
    expect(virtualRange(0,64,384,0,2)).toEqual({start:0,end:0,top:0});
    expect(virtualRange(10,64,128,99999,0)).toEqual({start:8,end:10,top:512});
    expect(()=>virtualRange(10,0,128,0,2)).toThrow();
  });
});
describe("anchored drawing navigation",()=>{
  it("fits without distortion and centers the remaining axis",()=>{
    expect(fitView({width:1000,height:500},{width:400,height:400},.05,8)).toEqual({x:0,y:100,scale:.4});
  });
  it("preserves the content coordinate under the anchor, including at scale limits",()=>{
    const view={x:-120,y:45,scale:1.5},anchor={x:222,y:108};
    for(const factor of [.001,.5,2,1000]){
      const next=zoomView(view,factor,anchor,.25,4);
      expect((anchor.x-next.x)/next.scale).toBeCloseTo((anchor.x-view.x)/view.scale);
      expect((anchor.y-next.y)/next.scale).toBeCloseTo((anchor.y-view.y)/view.scale);
      expect(next.scale).toBeGreaterThanOrEqual(.25);expect(next.scale).toBeLessThanOrEqual(4);
    }
    expect(()=>fitView({width:0,height:500},{width:400,height:400},.1,8)).toThrow();
  });
});
describe("provider-independent geography",()=>{
  it("prioritizes a selected location when small-scale labels collide",()=>{
    const points=geoData([{id:"a",label:"First location",longitude:0,latitude:0},{id:"b",label:"Selected location",longitude:1,latitude:1}],[]).points;
    expect([...geoLabelIds(points,"b",.3,8,16)]).toEqual(["b"]);
    expect(geoLabelIds(points,"b",8,8,16).has("b")).toBe(true);
  });
  it("projects WGS84 extrema and refuses out-of-range locations",()=>{
    expect(geoPosition(-180,90)).toEqual([0,0]);expect(geoPosition(180,-90)).toEqual([1000,500]);
    expect(()=>geoPosition(181,0)).toThrow();expect(()=>geoPosition(0,NaN)).toThrow();
  });
  it("splits the shortest Pacific crossing and validates endpoint identities",()=>{
    const points=[{id:"a",label:"A",longitude:170,latitude:0},{id:"b",label:"B",longitude:-170,latitude:0}];
    expect(geoData(points,[{id:"r",label:"A to B",source:"a",target:"b"}]).routes[0]!.paths).toHaveLength(2);
    expect(()=>geoData([...points,points[0]!],[])).toThrow();
    expect(()=>geoData(points,[{id:"r",label:"Missing",source:"a",target:"missing"}])).toThrow();
  });
  it("preserves polygon holes and permits null geometry without a dataset dependency",()=>{
    expect(geoLand()).toEqual([]);
    const paths=geoLand({type:"FeatureCollection",features:[{type:"Feature",geometry:null},{type:"Feature",geometry:{type:"Polygon",coordinates:[[[0,0],[10,0],[10,10],[0,0]],[[1,1],[2,1],[2,2],[1,1]]]}}]});
    expect(paths).toHaveLength(1);expect(paths[0]!.match(/M/g)).toHaveLength(2);expect(paths[0]!.match(/Z/g)).toHaveLength(2);
  });
});
