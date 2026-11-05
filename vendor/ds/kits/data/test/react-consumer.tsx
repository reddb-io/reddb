import Table from "../react/src/Table";
import TimeSeries from "../react/src/TimeSeries";
import CodeField from "../react/src/CodeField";
import TreeView from "../react/src/TreeView";
import VirtualList from "../react/src/VirtualList";
import TerminalSession from "../react/src/TerminalSession";
export const content=<><Table caption="Requests" columns={[{key:"path",header:"Path"}]} rows={[{path:"/records"}]} actions={context=><button>{context.rows.length}</button>} cell={context=><strong>{context.text}</strong>}/><TimeSeries label="Traffic" data={[{id:"ok",label:"Success",data:[{x:0,y:10}]}]} variant="stacked-area" onHiddenSeriesChange={ids=>ids.map(id=>id.toUpperCase())}/><CodeField label="Template" onValueChange={value=>value.toUpperCase()}/><TreeView label="Files" nodes={[]} leading={node=><span>{node.label}</span>} onReorder={move=>move.targetId.toUpperCase()}/><VirtualList label="Requests" items={[{id:"trace",duration:96}]} rowKey={item=>item.id}>{(item,index)=><button>{index}: {item.duration.toFixed(2)}</button>}</VirtualList><TerminalSession label="Shell" engine={async()=>({write(){},clear(){},focus(){},update(){},fit(){},dispose(){}})} onInteraction={event=>{if(event.type==="data")event.data.toUpperCase();}}/></>;
// @ts-expect-error Request items preserve their inferred shape.
export const invalid=<VirtualList label="Requests" items={[{id:"trace"}]} rowKey={item=>item.id}>{item=><p>{item.missing}</p>}</VirtualList>;
