import { unique, type VisualizationItem } from "./visualization";
export interface SequenceParticipant extends VisualizationItem {}
export interface SequenceMessage extends VisualizationItem { source:string;target:string;kind?:"call"|"return"|"async" }
export interface SequenceData { participants:readonly SequenceParticipant[];messages:readonly SequenceMessage[] }
export function sequenceData(data:SequenceData){unique(data.participants,"Sequence participants");unique(data.messages,"Sequence messages");const ids=new Set(data.participants.map(p=>p.id));for(const m of data.messages)if(!ids.has(m.source)||!ids.has(m.target))throw new Error("Unknown sequence participant");return data;}
