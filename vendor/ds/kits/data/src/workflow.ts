import type { Tone } from "@reddb-io/design-system/base";
import { normalizeRange } from "@reddb-io/design-system/base";
import { diagramRanks } from "./diagram";
import { short } from "./visualization";
import { uniqueEntities } from "./operational";
export interface WorkflowPort {
  id: string;
  label: string;
  side: "input" | "output";
}
export interface WorkflowNode {
  id: string;
  label: string;
  state: string;
  description?: string;
  progress?: number | null;
  tone?: Tone;
  ports?: readonly WorkflowPort[];
  group?: string;
}
export interface WorkflowEdge {
  id: string;
  label: string;
  source: string;
  target: string;
  sourcePort?: string;
  targetPort?: string;
  bus?: string;
  active?: boolean;
  packetLabel?: string;
}
export interface WorkflowData {
  nodes: readonly WorkflowNode[];
  edges: readonly WorkflowEdge[];
}
export const DEFAULT_PORTS: readonly WorkflowPort[] = [
  { id: "in", label: "Input", side: "input" },
  { id: "out", label: "Output", side: "output" },
];
export function workflowLayout(
  data: WorkflowData,
  direction: "right" | "down",
  unit = 32,
) {
  uniqueEntities([...data.nodes, ...data.edges], "WorkflowDiagram");
  const { rank, counts, levels, lanes } = diagramRanks(data),
    nodeWidth = unit * 7,
    nodeHeight = unit * 3.5,
    gap = unit * 2;
  const width =
      (direction === "right" ? levels : lanes) * (nodeWidth + gap) + gap,
    height =
      (direction === "right" ? lanes : levels) * (nodeHeight + gap) + gap,
    indices = new Map<number, number>();
  const nodes = data.nodes.map((n) => {
    if (
      n.progress !== undefined &&
      n.progress !== null &&
      !Number.isFinite(n.progress)
    )
      throw new Error(
        "WorkflowDiagram: progress must be finite, null or omitted",
      );
    const r = rank.get(n.id)!,
      lane = indices.get(r) ?? 0;
    indices.set(r, lane + 1);
    const ports = uniqueEntities(
      n.ports ?? DEFAULT_PORTS,
      `Workflow ports in ${n.id}`,
    );
    if (ports.some((p) => p.side !== "input" && p.side !== "output"))
      throw new Error("WorkflowDiagram: unknown port side");
    const x = gap + (direction === "right" ? r : lane) * (nodeWidth + gap),
      y = gap + (direction === "right" ? lane : r) * (nodeHeight + gap);
    return {
      ...n,
      x,
      y,
      width: nodeWidth,
      height: nodeHeight,
      paint: {
        inset: unit * 0.375,
        labelY: unit * 0.9,
        stateY: unit * 1.7,
        descriptionY: unit * 2.4,
        progressY: nodeHeight - unit * 0.45,
        trackHeight: unit * 0.125,
        radius: unit * 0.125,
        label: short(
          n.label,
          Math.max(8, Math.floor((nodeWidth - unit * 0.75) / 9)),
        ),
        description: short(
          n.description ?? "",
          Math.max(8, Math.floor((nodeWidth - unit * 0.75) / 7)),
        ),
      },
      ports,
      percent:
        n.progress == null ? null : normalizeRange(n.progress, 0, 100).percent,
    };
  });
  const byId = new Map(nodes.map((n) => [n.id, n])),
    channels = new Map<string, number>(),
    channelCounts = new Map<string, number>();
  for (const edge of data.edges) {
    if (edge.bus)
      channelCounts.set(edge.bus, (channelCounts.get(edge.bus) ?? 0) + 1);
  }
  const port = (
    node: (typeof nodes)[number],
    side: "input" | "output",
    id?: string,
  ) => {
    const ports = node.ports.filter((p) => p.side === side),
      at = id === undefined ? 0 : ports.findIndex((p) => p.id === id);
    if (at < 0 || !ports[at])
      throw new Error("WorkflowDiagram: connection references an unknown port");
    const p = ports[at]!;
    const fraction = (at + 1) / (ports.length + 1);
    return {
      id: p.id,
      x:
        node.x +
        (direction === "right"
          ? side === "input"
            ? 0
            : node.width
          : node.width * fraction),
      y:
        node.y +
        (direction === "right"
          ? node.height * fraction
          : side === "input"
            ? 0
            : node.height),
    };
  };

  const edges = data.edges.map((e) => {
    const a = port(byId.get(e.source)!, "output", e.sourcePort),
      b = port(byId.get(e.target)!, "input", e.targetPort),
      channel = e.bus ? (channels.get(e.bus) ?? 0) : 0;
    if (e.bus) channels.set(e.bus, channel + 1);
    // Center parallel channels inside the corridor; every bus stays bounded
    // regardless of its size, without collapsing channels after a fixed cap.
    const total = e.bus ? channelCounts.get(e.bus)! : 1;
    const offset = total > 1 ? (channel / (total - 1) - 0.5) * gap * 0.6 : 0;
    const backward = direction === "right" ? b.x <= a.x : b.y <= a.y;
    let path: string;
    if (backward) {
      const rail =
        direction === "right"
          ? Math.min(a.y, b.y) - gap * 0.5 - offset
          : Math.min(a.x, b.x) - gap * 0.5 - offset;
      path =
        direction === "right"
          ? `M${a.x},${a.y} H${a.x + unit / 2} V${rail} H${b.x - unit / 2} V${b.y} H${b.x}`
          : `M${a.x},${a.y} V${a.y + unit / 2} H${rail} V${b.y - unit / 2} H${b.x} V${b.y}`;
    } else {
      const mid =
        (direction === "right" ? (a.x + b.x) / 2 : (a.y + b.y) / 2) + offset;
      path =
        direction === "right"
          ? `M${a.x},${a.y} H${mid} V${b.y} H${b.x}`
          : `M${a.x},${a.y} V${mid} H${b.x} V${b.y}`;
    }
    return {
      ...e,
      path,
      start: a,
      end: b,
      channel,
      arrow:
        direction === "right"
          ? `M${b.x - 7},${b.y - 4} L${b.x},${b.y} L${b.x - 7},${b.y + 4}`
          : `M${b.x - 4},${b.y - 7} L${b.x},${b.y} L${b.x + 4},${b.y - 7}`,
      labelX: (a.x + b.x) / 2,
      labelY: Math.min(a.y, b.y) - 12,
    };
  });
  const related = (id: string) =>
    new Set([
      id,
      ...data.edges
        .filter((e) => e.source === id || e.target === id)
        .flatMap((e) => [e.source, e.target, e.id]),
    ]);
  return {
    nodes: nodes.map((node) => ({
      ...node,
      portPositions: node.ports.map((p) => ({
        ...p,
        ...port(node, p.side, p.id),
      })),
    })),
    edges,
    width,
    height,
    nodeWidth,
    nodeHeight,
    related,
  };
}

export function workflowFocus(
  scene: ReturnType<typeof workflowLayout>,
  id: string | null,
) {
  const node = scene.nodes.find((n) => n.id === id);
  return node
    ? {
        x: node.x - 24,
        y: node.y - 24,
        width: node.width + 48,
        height: node.height + 48,
      }
    : null;
}
export function workflowNodeLabel(node: WorkflowNode) {
  return [
    node.label,
    node.state,
    node.description,
    node.progress == null ? undefined : `${Math.round(node.progress)}%`,
  ]
    .filter(Boolean)
    .join(", ");
}
