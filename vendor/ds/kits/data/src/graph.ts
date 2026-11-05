/** Pure normalization, structural analysis, grouping, and search for the Data Kit network Graph. */

import leiden from "@aflsolutions/graphology-communities-leiden";
import Graphology from "graphology";

export const GRAPH_COLOR_TOKENS = [
  "var(--reddb-color-series-1)",
  "var(--reddb-color-series-2)",
  "var(--reddb-color-series-3)",
  "var(--reddb-color-series-4)",
  "var(--reddb-color-series-5)",
  "var(--reddb-color-series-6)",
] as const;

export const GRAPH_EDGE_COLOR_TOKEN = "var(--reddb-color-control-edge)" as const;
export const GRAPH_MUTED_COLOR_TOKEN = "var(--reddb-color-ink-muted)" as const;
export const GRAPH_FOCUSED_SCALE = 1.3;
export const GRAPH_FOCUS_PULSE_SCALE = 0.05;
export const GRAPH_NEIGHBOUR_SCALE = 1.12;
export const GRAPH_REST_SCALE = 0.9;
export const GRAPH_NEIGHBOUR_LABEL_LIMIT = 36;
export const GRAPH_EDGE_LABEL_MAX_LENGTH = 28;
export const GRAPH_LAYOUT_MIN_ITERATIONS = 20;
export const GRAPH_LAYOUT_MAX_ITERATIONS = 100;
export const GRAPH_LAYOUT_ITERATIONS_PER_NODE = 2;
export const GRAPH_LAYOUT_GRAVITY = 0.08;
export const GRAPH_LAYOUT_SCALING_RATIO = 8;
/** Minimum rendered point-cloud span on each usable canvas axis after fitting. */
export const GRAPH_LAYOUT_MIN_AXIS_FILL = 0.72;
export const GRAPH_SUPERNODE_BASE_SIZE = 12;
export const GRAPH_SUPERNODE_MEMBER_SCALE = 4;
export const GRAPH_ENGINE_STAGE_PADDING = 24;

export type GraphMetadata = Record<string, unknown>;

export type GraphColorToken =
  | (typeof GRAPH_COLOR_TOKENS)[number]
  | typeof GRAPH_EDGE_COLOR_TOKEN
  | typeof GRAPH_MUTED_COLOR_TOKEN;

export interface GraphNode {
  /** Stable node identity. */
  id: string;
  label?: string;
  /** Caller-owned community identity. Omit it to let the engine detect communities. */
  community?: string;
  /** Optional semantic DS token override. Raw colors are intentionally unsupported. */
  color?: GraphColorToken;
  size?: number;
  x?: number;
  y?: number;
  /** Caller-owned domain data. The design system transports but never interprets it. */
  metadata?: GraphMetadata;
}

export interface GraphEdge {
  id?: string;
  source: string;
  target: string;
  label?: string;
  weight?: number;
  /** Optional semantic DS token override. Raw colors are intentionally unsupported. */
  color?: GraphColorToken;
  /** Caller-owned domain data. The design system transports but never interprets it. */
  metadata?: GraphMetadata;
}

export interface GraphData {
  nodes: readonly GraphNode[];
  edges: readonly GraphEdge[];
}

export interface ShapedGraphNode {
  id: string;
  label: string;
  community: string;
  communityExplicit: boolean;
  color: GraphColorToken;
  size: number;
  x: number;
  y: number;
  metadata?: GraphMetadata;
  /** Structural path-carrier selected from backbone betweenness. */
  isTrunk?: boolean;
  /** Zero-based backbone-betweenness priority among selected trunks. */
  trunkRank?: number;
  /** Present only when this node represents a collapsed community. */
  collapsedCommunity?: string;
  /** Present only when this node summarises a deep layout subtree. */
  collapsedSubtree?: string;
  memberIds?: readonly string[];
}

export interface ShapedGraphEdge {
  id: string;
  source: string;
  target: string;
  label?: string;
  weight: number;
  color: GraphColorToken;
  metadata?: GraphMetadata;
}

export interface ShapedGraphData {
  nodes: readonly ShapedGraphNode[];
  edges: readonly ShapedGraphEdge[];
}

export interface FocusedGraphNode extends ShapedGraphNode {
  scale: number;
  showLabel: boolean;
}

export interface FocusedGraphEdge extends ShapedGraphEdge {
  displayLabel: string | undefined;
}

export interface FocusedGraphData {
  nodes: readonly FocusedGraphNode[];
  edges: readonly FocusedGraphEdge[];
}

export interface GraphFocusViewport {
  width: number;
  height: number;
  padding?: number;
  /** Minimum clear space retained while focus scales node circles. */
  nodeMargin?: number;
}

export interface GraphCommunity {
  id: string;
  label: string;
  color: GraphColorToken;
  nodes: readonly ShapedGraphNode[];
}

export interface GraphPoint {
  x: number;
  y: number;
}

interface GraphEdgePath extends ShapedGraphEdge {
  points: readonly GraphPoint[];
}

export interface GraphInteraction {
  kind: "node" | "edge";
  action: "click" | "focus";
  id: string;
  metadata?: GraphMetadata;
}

export interface GraphCentralityLayoutOptions {
  /** Stable integer used to rotate and order otherwise equivalent branches. */
  seed?: number;
  /** Override automatic centrality-based hub selection for a known topology. */
  hubCount?: number;
}

export interface GraphCentralityLayout {
  data: ShapedGraphData;
  hubs: readonly string[];
  parentByNode: Readonly<Record<string, string | null>>;
  childrenByNode: Readonly<Record<string, readonly string[]>>;
}

export interface GraphCentralityPartitionWork {
  edgeVisits: number;
  queuePushes: number;
  queuePops: number;
}

export interface GraphCentralityPartition {
  parentByNode: Readonly<Record<string, string | null>>;
  childrenByNode: Readonly<Record<string, readonly string[]>>;
  ownerByNode: Readonly<Record<string, string>>;
  work: GraphCentralityPartitionWork;
}

export interface GraphSparseStressLayoutOptions {
  /** Stable integer used for pivot selection and pair shuffling. */
  seed?: number;
  /** Override the centrality-forest hub count used for initialisation. */
  hubCount?: number;
  /** Number of max-min pivots. Defaults to at most 32 and O(sqrt(nodes)). */
  pivotCount?: number;
  /** Number of exponentially annealed SGD passes over the sparse pair set. */
  epochs?: number;
  /** Render viewport used to keep node radii apart in pixel space. */
  viewport?: GraphFocusViewport;
  /** Minimum clear space between rendered node circles. */
  nodeMargin?: number;
}

export interface GraphSparseStressLayout {
  data: ShapedGraphData;
  pivots: readonly string[];
  epochs: number;
  pairCount: number;
  parentByNode: Readonly<Record<string, string | null>>;
  childrenByNode: Readonly<Record<string, readonly string[]>>;
}

export interface GraphLevelOfDetailOptions extends GraphFocusViewport {
  /** Subtree roots whose summary nodes have been expanded by the user. */
  expandedSubtrees?: Iterable<string>;
}

export interface GraphLevelOfDetailProfile {
  nodeLimit: number;
  nodeScale: number;
  radiusStep: number;
  labelLimit: number;
}

export interface GraphLevelOfDetail extends GraphLevelOfDetailProfile {
  data: ShapedGraphData;
  sourceNodeCount: number;
}

export interface GraphCommunityDetectionOptions {
  /** Stable integer used by Leiden when visiting equivalent candidates. */
  seed?: number;
}

export interface GraphBackboneOptions {
  /** Disparity-filter significance threshold. Defaults to 0.5. */
  alpha?: number;
}

export interface GraphBetweennessOptions {
  /** Stable integer used to sample source nodes. */
  seed?: number;
  /** Brandes source count. Small graphs default to exact ranking. */
  sampleSize?: number;
}

export interface GraphBetweennessRank {
  id: string;
  score: number;
}

export interface GraphStructureAnalysisOptions
  extends GraphBackboneOptions, GraphBetweennessOptions {}

export interface GraphStructureAnalysis {
  data: ShapedGraphData;
  backbone: ShapedGraphData;
  ranking: readonly GraphBetweennessRank[];
  trunks: readonly string[];
}

const UNASSIGNED_COMMUNITY = "unassigned";

function finite(value: number | undefined, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function rounded(value: number): number {
  return Math.round(value * 1_000_000) / 1_000_000;
}

function truncateEdgeLabel(label: string): string {
  return label.length <= GRAPH_EDGE_LABEL_MAX_LENGTH
    ? label
    : `${label.slice(0, GRAPH_EDGE_LABEL_MAX_LENGTH - 1)}…`;
}

/**
 * Copy caller data into the minimal immutable shape shared by SVG and Sigma.
 * Missing positions use a deterministic circle so the first paint is stable.
 */
export function shapeGraphData(data: GraphData): ShapedGraphData {
  const ids = new Set<string>();
  const communities = new Map<string, number>();

  for (const node of data.nodes) {
    if (ids.has(node.id)) throw new RangeError(`Duplicate graph node: ${node.id}`);
    ids.add(node.id);
    const community = node.community ?? UNASSIGNED_COMMUNITY;
    if (!communities.has(community)) communities.set(community, communities.size);
  }

  const count = data.nodes.length;
  const nodes = data.nodes.map((node, index): ShapedGraphNode => {
    const angle = count === 0 ? 0 : (index / count) * Math.PI * 2;
    const community = node.community ?? UNASSIGNED_COMMUNITY;
    const communityIndex = communities.get(community) ?? 0;
    return {
      id: node.id,
      label: node.label ?? node.id,
      community,
      communityExplicit: node.community !== undefined,
      color: node.color ?? GRAPH_COLOR_TOKENS[communityIndex % GRAPH_COLOR_TOKENS.length]!,
      size: Math.max(1, finite(node.size, 8)),
      x: rounded(finite(node.x, Math.cos(angle))),
      y: rounded(finite(node.y, Math.sin(angle))),
      ...(node.metadata === undefined ? {} : { metadata: node.metadata }),
    };
  });

  const edgeIds = new Set<string>();
  const edges = data.edges.map((edge, index): ShapedGraphEdge => {
    if (!ids.has(edge.source) || !ids.has(edge.target)) {
      throw new RangeError(
        `Graph edge ${edge.source}→${edge.target} references an unknown node.`,
      );
    }
    const id = edge.id ?? `${edge.source}→${edge.target}:${index}`;
    if (edgeIds.has(id)) throw new RangeError(`Duplicate graph edge: ${id}`);
    edgeIds.add(id);
    return {
      id,
      source: edge.source,
      target: edge.target,
      ...(edge.label === undefined ? {} : { label: edge.label }),
      weight: Math.max(0, finite(edge.weight, 1)),
      color: edge.color ?? GRAPH_EDGE_COLOR_TOKEN,
      ...(edge.metadata === undefined ? {} : { metadata: edge.metadata }),
    };
  });

  return { nodes, edges };
}

interface GraphNeighbour {
  id: string;
  weight: number;
}

function graphAdjacency(data: ShapedGraphData): Map<string, GraphNeighbour[]> {
  const adjacency = new Map(data.nodes.map((node) => [node.id, [] as GraphNeighbour[]]));
  for (const edge of data.edges) {
    adjacency.get(edge.source)!.push({ id: edge.target, weight: edge.weight });
    adjacency.get(edge.target)!.push({ id: edge.source, weight: edge.weight });
  }
  return adjacency;
}

function stableUnit(seed: number, value: string): number {
  let hash = (2166136261 ^ seed) >>> 0;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 16777619) >>> 0;
  }
  return hash / 0x1_0000_0000;
}

interface LayoutQueueEntry {
  id: string;
  distance: number;
  owner: string;
}

function pushLayoutQueue(
  queue: LayoutQueueEntry[],
  entry: LayoutQueueEntry,
  compare: (left: LayoutQueueEntry, right: LayoutQueueEntry) => number,
): void {
  queue.push(entry);
  let index = queue.length - 1;
  while (index > 0) {
    const parent = Math.floor((index - 1) / 2);
    if (compare(queue[parent]!, entry) <= 0) break;
    queue[index] = queue[parent]!;
    index = parent;
  }
  queue[index] = entry;
}

function popLayoutQueue(
  queue: LayoutQueueEntry[],
  compare: (left: LayoutQueueEntry, right: LayoutQueueEntry) => number,
): LayoutQueueEntry | undefined {
  const first = queue[0];
  const last = queue.pop();
  if (queue.length === 0 || last === undefined) return first;
  let index = 0;
  while (true) {
    const left = index * 2 + 1;
    if (left >= queue.length) break;
    const right = left + 1;
    const child = right < queue.length && compare(queue[right]!, queue[left]!) < 0 ? right : left;
    if (compare(last, queue[child]!) <= 0) break;
    queue[index] = queue[child]!;
    index = child;
  }
  queue[index] = last;
  return first;
}

/** Partition a weighted graph between ranked hubs with a binary min-heap. */
export function partitionGraphByCentrality(
  data: ShapedGraphData,
  hubs: readonly string[],
  options: Pick<GraphCentralityLayoutOptions, "seed"> = {},
): GraphCentralityPartition {
  if (data.nodes.length === 0) {
    return {
      parentByNode: {},
      childrenByNode: {},
      ownerByNode: {},
      work: { edgeVisits: 0, queuePushes: 0, queuePops: 0 },
    };
  }
  if (hubs.length === 0) throw new RangeError("Graph partition requires at least one hub.");

  const ids = new Set(data.nodes.map((node) => node.id));
  for (const hub of hubs) {
    if (!ids.has(hub)) throw new RangeError(`Unknown graph partition hub: ${hub}`);
  }

  const seed = Math.trunc(options.seed ?? 0);
  const adjacency = graphAdjacency(data);
  for (const neighbours of adjacency.values()) {
    neighbours.sort((left, right) => right.weight - left.weight || left.id.localeCompare(right.id));
  }
  const hubSet = new Set(hubs);
  const hubRank = new Map(hubs.map((hub, index) => [hub, index]));
  const distance = new Map<string, number>();
  const owner = new Map<string, string>();
  const parent = new Map<string, string | null>();
  const settled = new Set<string>();
  const work = { edgeVisits: 0, queuePushes: 0, queuePops: 0 };

  for (const hub of hubs) {
    distance.set(hub, 0);
    owner.set(hub, hub);
    parent.set(hub, null);
  }

  const compareQueue = (left: LayoutQueueEntry, right: LayoutQueueEntry): number => {
    const byDistance = left.distance - right.distance;
    if (Math.abs(byDistance) > 1e-9) return byDistance;
    const byHub = hubRank.get(left.owner)! - hubRank.get(right.owner)!;
    return byHub || left.id.localeCompare(right.id);
  };
  const queue: LayoutQueueEntry[] = [];
  for (const hub of hubs) {
    pushLayoutQueue(queue, { id: hub, distance: 0, owner: hub }, compareQueue);
    work.queuePushes += 1;
  }

  while (queue.length > 0) {
    const entry = popLayoutQueue(queue, compareQueue)!;
    work.queuePops += 1;
    const next = entry.id;
    if (
      settled.has(next) ||
      Math.abs(entry.distance - (distance.get(next) ?? Number.POSITIVE_INFINITY)) > 1e-9 ||
      entry.owner !== owner.get(next)
    ) continue;
    settled.add(next);
    const nextOwner = owner.get(next)!;
    for (const neighbour of adjacency.get(next) ?? []) {
      work.edgeVisits += 1;
      if (hubSet.has(neighbour.id) && neighbour.id !== nextOwner) continue;
      const candidate = distance.get(next)! + 1 / Math.max(0.01, neighbour.weight);
      const current = distance.get(neighbour.id);
      const currentOwner = owner.get(neighbour.id);
      const winsTie =
        current !== undefined &&
        Math.abs(candidate - current) <= 1e-9 &&
        hubRank.get(nextOwner)! < hubRank.get(currentOwner!)!;
      if (current === undefined || candidate < current - 1e-9 || winsTie) {
        distance.set(neighbour.id, candidate);
        owner.set(neighbour.id, nextOwner);
        parent.set(neighbour.id, next);
        pushLayoutQueue(
          queue,
          { id: neighbour.id, distance: candidate, owner: nextOwner },
          compareQueue,
        );
        work.queuePushes += 1;
      }
    }
  }

  for (const node of data.nodes) {
    if (owner.has(node.id)) continue;
    owner.set(node.id, hubs[0]!);
    parent.set(node.id, hubs[0]!);
  }

  const children = new Map(data.nodes.map((node) => [node.id, [] as string[]]));
  for (const node of data.nodes) {
    const ancestor = parent.get(node.id) ?? null;
    if (ancestor !== null) children.get(ancestor)!.push(node.id);
  }
  for (const [id, descendants] of children) {
    descendants.sort((left, right) =>
      stableUnit(seed, `${id}:${left}`) - stableUnit(seed, `${id}:${right}`) ||
      left.localeCompare(right),
    );
  }

  return {
    parentByNode: Object.fromEntries(
      data.nodes.map((node) => [node.id, parent.get(node.id) ?? null]),
    ),
    childrenByNode: Object.fromEntries(
      data.nodes.map((node) => [node.id, children.get(node.id) ?? []]),
    ),
    ownerByNode: Object.fromEntries(data.nodes.map((node) => [node.id, owner.get(node.id)!])),
    work,
  };
}

/**
 * Seed a hub-first spanning forest. Hubs occupy a wide outer ring while each
 * weighted subtree receives a non-overlapping angular sector around its hub.
 * The result is pure and deterministic, so SVG and the lazy engine start from
 * exactly the same topology-aware positions.
 */
export function layoutGraphByCentrality(
  data: ShapedGraphData,
  options: GraphCentralityLayoutOptions = {},
): GraphCentralityLayout {
  if (data.nodes.length === 0) {
    return { data, hubs: [], parentByNode: {}, childrenByNode: {} };
  }

  const seed = Math.trunc(options.seed ?? 0);
  const structural = analyseGraphStructure(data, { seed });
  data = structural.data;
  const ranked = structural.ranking.map((entry) => entry.id);
  const hubs = options.hubCount === undefined
    ? [...structural.trunks]
    : ranked.slice(0, Math.max(1, Math.min(ranked.length, Math.trunc(options.hubCount))));
  const partition = partitionGraphByCentrality(data, hubs, { seed });
  const children = new Map(
    data.nodes.map((node) => [node.id, [...(partition.childrenByNode[node.id] ?? [])]]),
  );

  const positions = new Map<string, GraphPoint>();
  const hubRadius = hubs.length === 1 ? 0 : 0.64;
  const hubRotation = stableUnit(seed, "hubs") * Math.PI * 2;
  const hubOrbit = hubs.length === 1 ? 0.82 : Math.min(0.3, 0.56 / Math.sqrt(hubs.length));

  for (let hubIndex = 0; hubIndex < hubs.length; hubIndex += 1) {
    const hub = hubs[hubIndex]!;
    const hubAngle = hubRotation + (hubIndex / hubs.length) * Math.PI * 2;
    const centre = {
      x: rounded(Math.cos(hubAngle) * hubRadius),
      y: rounded(Math.sin(hubAngle) * hubRadius),
    };
    positions.set(hub, centre);
    const order = [hub];
    const depthByNode = new Map<string, number>([[hub, 0]]);
    for (let index = 0; index < order.length; index += 1) {
      const node = order[index]!;
      for (const child of children.get(node) ?? []) {
        depthByNode.set(child, depthByNode.get(node)! + 1);
        order.push(child);
      }
    }
    const leafCountByNode = new Map<string, number>();
    for (let index = order.length - 1; index >= 0; index -= 1) {
      const node = order[index]!;
      const descendants = children.get(node) ?? [];
      leafCountByNode.set(
        node,
        descendants.length === 0
          ? 1
          : descendants.reduce((total, child) => total + leafCountByNode.get(child)!, 0),
      );
    }
    const depthLimit = Math.max(1, ...depthByNode.values());
    const branchRotation = hubAngle + stableUnit(seed, `branches:${hub}`) * Math.PI * 2;
    const pending = [{ node: hub, start: branchRotation, end: branchRotation + Math.PI * 2 }];
    while (pending.length > 0) {
      const { node, start, end } = pending.pop()!;
      const depth = depthByNode.get(node)!;
      if (node !== hub) {
        const angle = (start + end) / 2;
        const radius = hubOrbit * (0.3 + (0.7 * depth) / depthLimit);
        positions.set(node, {
          x: rounded(centre.x + Math.cos(angle) * radius),
          y: rounded(centre.y + Math.sin(angle) * radius),
        });
      }
      const descendants = children.get(node) ?? [];
      const leaves = descendants.reduce(
        (total, child) => total + leafCountByNode.get(child)!,
        0,
      );
      let cursor = start;
      const childRanges: Array<{ node: string; start: number; end: number }> = [];
      for (const child of descendants) {
        const span = leaves === 0 ? 0 : ((end - start) * leafCountByNode.get(child)!) / leaves;
        childRanges.push({ node: child, start: cursor, end: cursor + span });
        cursor += span;
      }
      for (let index = childRanges.length - 1; index >= 0; index -= 1) {
        pending.push(childRanges[index]!);
      }
    }
  }

  return {
    data: {
      nodes: data.nodes.map((node) => ({ ...node, ...(positions.get(node.id) ?? node) })),
      edges: data.edges,
    },
    hubs,
    parentByNode: partition.parentByNode,
    childrenByNode: partition.childrenByNode,
  };
}

interface SparseStressPair {
  left: number;
  right: number;
  target: number;
  weight: number;
}

function seededRandom(seed: number): () => number {
  let state = (Math.trunc(seed) ^ 0x9e3779b9) >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let value = state;
    value = Math.imul(value ^ (value >>> 15), value | 1);
    value ^= value + Math.imul(value ^ (value >>> 7), value | 61);
    return ((value ^ (value >>> 14)) >>> 0) / 0x1_0000_0000;
  };
}

/**
 * Detect well-connected communities with Leiden while preserving caller-owned
 * communities. Detected identities and colors are stable for a fixed seed.
 */
export function detectGraphCommunities(
  data: ShapedGraphData,
  options: GraphCommunityDetectionOptions = {},
): ShapedGraphData {
  if (
    data.nodes.length < 2 ||
    data.edges.length === 0 ||
    data.nodes.some((node) => node.communityExplicit)
  ) return data;

  const graph = new Graphology({ type: "undirected", multi: true });
  for (const node of data.nodes) graph.addNode(node.id);
  for (const edge of data.edges) {
    if (edge.source === edge.target) continue;
    graph.addEdgeWithKey(edge.id, edge.source, edge.target, { weight: edge.weight });
  }
  if (graph.size === 0) return data;

  leiden.assign(graph, {
    attributes: { community: "community", weight: "weight" },
    rng: seededRandom(Math.trunc(options.seed ?? 0)),
    weighted: true,
  });

  const stableCommunities = new Map<unknown, string>();
  return {
    nodes: data.nodes.map((node) => {
      const detected = graph.getNodeAttribute(node.id, "community") as unknown;
      let community = stableCommunities.get(detected);
      if (community === undefined) {
        community = `community-${stableCommunities.size + 1}`;
        stableCommunities.set(detected, community);
      }
      const communityIndex = stableCommunities.size - 1;
      return {
        ...node,
        community,
        color: GRAPH_COLOR_TOKENS[communityIndex % GRAPH_COLOR_TOKENS.length]!,
      };
    }),
    edges: data.edges,
  };
}

/**
 * Extract Serrano et al.'s multiscale backbone. An undirected edge survives
 * when its normalised weight is significant for at least one endpoint.
 */
export function filterGraphBackbone(
  data: ShapedGraphData,
  options: GraphBackboneOptions = {},
): ShapedGraphData {
  const alpha = options.alpha ?? 0.5;
  if (!(alpha > 0 && alpha <= 1)) {
    throw new RangeError("Graph backbone alpha must be greater than 0 and at most 1.");
  }

  const degree = new Map(data.nodes.map((node) => [node.id, 0]));
  const strength = new Map(data.nodes.map((node) => [node.id, 0]));
  for (const edge of data.edges) {
    if (edge.source === edge.target || edge.weight <= 0) continue;
    degree.set(edge.source, degree.get(edge.source)! + 1);
    degree.set(edge.target, degree.get(edge.target)! + 1);
    strength.set(edge.source, strength.get(edge.source)! + edge.weight);
    strength.set(edge.target, strength.get(edge.target)! + edge.weight);
  }

  const significance = (node: string, weight: number): number => {
    const neighbours = degree.get(node) ?? 0;
    const total = strength.get(node) ?? 0;
    if (neighbours <= 1) return 0;
    if (total <= 0) return 1;
    return Math.pow(Math.max(0, 1 - weight / total), neighbours - 1);
  };

  return {
    nodes: data.nodes,
    edges: data.edges.filter((edge) =>
      Math.min(
        significance(edge.source, edge.weight),
        significance(edge.target, edge.weight),
      ) < alpha,
    ),
  };
}

/** Rank nodes by sampled Brandes betweenness over an undirected graph. */
export function rankGraphByBetweenness(
  data: ShapedGraphData,
  options: GraphBetweennessOptions = {},
): readonly GraphBetweennessRank[] {
  const ids = data.nodes.map((node) => node.id);
  const indexById = new Map(ids.map((id, index) => [id, index]));
  const neighbours = Array.from({ length: ids.length }, () => new Set<number>());
  for (const edge of data.edges) {
    const source = indexById.get(edge.source)!;
    const target = indexById.get(edge.target)!;
    if (source === target) continue;
    neighbours[source]!.add(target);
    neighbours[target]!.add(source);
  }

  const requestedSamples = options.sampleSize ?? (ids.length <= 128
    ? ids.length
    : Math.max(32, Math.ceil(Math.sqrt(ids.length))));
  const sampleSize = Math.max(0, Math.min(ids.length, Math.trunc(requestedSamples)));
  const sources = ids.map((_, index) => index);
  const random = seededRandom(Math.trunc(options.seed ?? 0));
  for (let index = sources.length - 1; index > 0; index -= 1) {
    const other = Math.floor(random() * (index + 1));
    [sources[index], sources[other]] = [sources[other]!, sources[index]!];
  }
  sources.length = sampleSize;

  const scores = new Float64Array(ids.length);
  for (const source of sources) {
    const stack: number[] = [];
    const predecessors = Array.from({ length: ids.length }, () => [] as number[]);
    const paths = new Float64Array(ids.length);
    const distance = new Int32Array(ids.length);
    distance.fill(-1);
    paths[source] = 1;
    distance[source] = 0;
    const queue = new Int32Array(ids.length);
    queue[0] = source;
    let read = 0;
    let write = 1;

    while (read < write) {
      const node = queue[read++]!;
      stack.push(node);
      for (const neighbour of neighbours[node]!) {
        if (distance[neighbour]! < 0) {
          distance[neighbour] = distance[node]! + 1;
          queue[write++] = neighbour;
        }
        if (distance[neighbour] === distance[node]! + 1) {
          paths[neighbour] = paths[neighbour]! + paths[node]!;
          predecessors[neighbour]!.push(node);
        }
      }
    }

    const dependency = new Float64Array(ids.length);
    while (stack.length > 0) {
      const node = stack.pop()!;
      for (const predecessor of predecessors[node]!) {
        dependency[predecessor] = dependency[predecessor]! +
          (paths[predecessor]! / paths[node]!) * (1 + dependency[node]!);
      }
      if (node !== source) scores[node] = scores[node]! + dependency[node]!;
    }
  }

  const scale = sampleSize === 0 ? 0 : ids.length / (sampleSize * 2);
  const strength = new Map(ids.map((id) => [id, 0]));
  for (const edge of data.edges) {
    strength.set(edge.source, strength.get(edge.source)! + edge.weight);
    strength.set(edge.target, strength.get(edge.target)! + edge.weight);
  }
  return ids
    .map((id, index) => ({ id, score: rounded(scores[index]! * scale) }))
    .sort((left, right) =>
      right.score - left.score ||
      strength.get(right.id)! - strength.get(left.id)! ||
      left.id.localeCompare(right.id),
    );
}

/**
 * Run the structural pipeline used by layout and visual hierarchy. Trunk
 * count follows Leiden's community count rather than a fixed presentation cap.
 */
export function analyseGraphStructure(
  data: ShapedGraphData,
  options: GraphStructureAnalysisOptions = {},
): GraphStructureAnalysis {
  const detected = detectGraphCommunities(data, { seed: options.seed });
  const backbone = filterGraphBackbone(detected, { alpha: options.alpha });
  const ranking = rankGraphByBetweenness(backbone, {
    seed: options.seed,
    sampleSize: options.sampleSize,
  });
  const communityCount = new Set(detected.nodes.map((node) => node.community)).size;
  const trunks = ranking.slice(0, communityCount).map((entry) => entry.id);
  const trunkRank = new Map(trunks.map((id, index) => [id, index]));
  const dataWithHierarchy: ShapedGraphData = {
    nodes: detected.nodes.map((node) => {
      const rank = trunkRank.get(node.id);
      if (rank === undefined) return node;
      const emphasis = 1.08 + 0.12 * (1 - rank / Math.max(1, trunks.length));
      return {
        ...node,
        isTrunk: true,
        trunkRank: rank,
        size: rounded(node.size * emphasis),
      };
    }),
    edges: detected.edges,
  };
  const hierarchyNodes = new Map(dataWithHierarchy.nodes.map((node) => [node.id, node]));
  return {
    data: dataWithHierarchy,
    backbone: {
      nodes: backbone.nodes.map((node) => hierarchyNodes.get(node.id)!),
      edges: backbone.edges,
    },
    ranking,
    trunks,
  };
}

function indexedAdjacency(data: ShapedGraphData): number[][] {
  const indexById = new Map(data.nodes.map((node, index) => [node.id, index]));
  const adjacency = Array.from({ length: data.nodes.length }, () => [] as number[]);
  for (const edge of data.edges) {
    const source = indexById.get(edge.source)!;
    const target = indexById.get(edge.target)!;
    if (source === target) continue;
    adjacency[source]!.push(target);
    adjacency[target]!.push(source);
  }
  for (const neighbours of adjacency) neighbours.sort((left, right) => left - right);
  return adjacency;
}

function breadthFirstDistances(adjacency: readonly number[][], source: number): Int32Array {
  const distances = new Int32Array(adjacency.length);
  distances.fill(-1);
  distances[source] = 0;
  const queue = new Int32Array(adjacency.length);
  queue[0] = source;
  let read = 0;
  let write = 1;
  while (read < write) {
    const node = queue[read++]!;
    const nextDistance = distances[node]! + 1;
    for (const neighbour of adjacency[node]!) {
      if (distances[neighbour] !== -1) continue;
      distances[neighbour] = nextDistance;
      queue[write++] = neighbour;
    }
  }
  return distances;
}

function sparseStressPairs(
  data: ShapedGraphData,
  pivotCount: number,
  random: () => number,
): { pairs: SparseStressPair[]; pivots: number[] } {
  const adjacency = indexedAdjacency(data);
  if (data.nodes.length < 2) return { pairs: [], pivots: data.nodes.length === 0 ? [] : [0] };
  const selected = new Set<number>();
  const pivots: number[] = [];
  const distancesByPivot: Int32Array[] = [];
  const minimumDistance = new Float64Array(data.nodes.length);
  minimumDistance.fill(Number.POSITIVE_INFINITY);
  let next = Math.floor(random() * data.nodes.length);

  while (pivots.length < Math.min(pivotCount, data.nodes.length)) {
    pivots.push(next);
    selected.add(next);
    const distances = breadthFirstDistances(adjacency, next);
    distancesByPivot.push(distances);
    for (let node = 0; node < data.nodes.length; node += 1) {
      const distance = distances[node]!;
      if (distance >= 0 && distance < minimumDistance[node]!) minimumDistance[node] = distance;
    }
    let farthest = -1;
    let farthestDistance = -1;
    for (let node = 0; node < data.nodes.length; node += 1) {
      if (selected.has(node)) continue;
      const distance = minimumDistance[node]!;
      if (distance > farthestDistance) {
        farthest = node;
        farthestDistance = distance;
      }
    }
    if (farthest < 0) break;
    next = farthest;
  }

  const pairs: SparseStressPair[] = [];
  const keys = new Set<string>();
  const addPair = (left: number, right: number, target: number): void => {
    if (left === right || target <= 0) return;
    const first = Math.min(left, right);
    const second = Math.max(left, right);
    const key = `${first}:${second}`;
    if (keys.has(key)) return;
    keys.add(key);
    pairs.push({ left: first, right: second, target, weight: 1 / (target * target) });
  };
  for (let pivotIndex = 0; pivotIndex < pivots.length; pivotIndex += 1) {
    const pivot = pivots[pivotIndex]!;
    const distances = distancesByPivot[pivotIndex]!;
    for (let node = 0; node < data.nodes.length; node += 1) {
      if (distances[node]! > 0) addPair(pivot, node, distances[node]!);
    }
  }
  const indexById = new Map(data.nodes.map((node, index) => [node.id, index]));
  for (const edge of data.edges) addPair(indexById.get(edge.source)!, indexById.get(edge.target)!, 1);
  return { pairs, pivots };
}

function positionedGraph(
  data: ShapedGraphData,
  x: ArrayLike<number>,
  y: ArrayLike<number>,
): ShapedGraphData {
  if (data.nodes.length === 0) return data;
  let centreX = 0;
  let centreY = 0;
  for (let index = 0; index < data.nodes.length; index += 1) {
    centreX += x[index]!;
    centreY += y[index]!;
  }
  centreX /= data.nodes.length;
  centreY /= data.nodes.length;
  let extent = 0;
  for (let index = 0; index < data.nodes.length; index += 1) {
    extent = Math.max(extent, Math.abs(x[index]! - centreX), Math.abs(y[index]! - centreY));
  }
  const scale = extent > 1e-12 ? 0.92 / extent : 1;
  return {
    nodes: data.nodes.map((node, index) => ({
      ...node,
      // Final layout precision distinguishes dense equivalent branches; fixed
      // decimal quantisation stacks otherwise distinct nodes at large counts.
      x: (x[index]! - centreX) * scale,
      y: (y[index]! - centreY) * scale,
    })),
    edges: data.edges,
  };
}

function interpolateGraphs(
  data: ShapedGraphData,
  initial: ShapedGraphData,
  candidate: ShapedGraphData,
  progress: number,
): ShapedGraphData {
  const x = new Float64Array(data.nodes.length);
  const y = new Float64Array(data.nodes.length);
  for (let index = 0; index < data.nodes.length; index += 1) {
    x[index] = initial.nodes[index]!.x * (1 - progress) + candidate.nodes[index]!.x * progress;
    y[index] = initial.nodes[index]!.y * (1 - progress) + candidate.nodes[index]!.y * progress;
  }
  return positionedGraph(data, x, y);
}

const GRAPH_LABEL_CHARACTER_WIDTH = 7.5;
const GRAPH_LABEL_MIN_WIDTH = 16;
const GRAPH_LABEL_FOOTPRINT_RADIUS_LIMIT = 2;
const GRAPH_NODE_GAP_REFERENCE_RADIUS = 8;
const GRAPH_FOCUS_RING_CLEARANCE = 3;

function graphNodeFootprintRadius(node: ShapedGraphNode): number {
  const labelRadius =
    Math.max(GRAPH_LABEL_MIN_WIDTH, node.label.length * GRAPH_LABEL_CHARACTER_WIDTH) / 2;
  return Math.max(
    node.size * (GRAPH_FOCUSED_SCALE + GRAPH_FOCUS_PULSE_SCALE),
    Math.min(node.size * GRAPH_LABEL_FOOTPRINT_RADIUS_LIMIT, labelRadius),
  );
}

function graphNodeGap(
  leftRadius: number,
  rightRadius: number,
  nodeMargin: number,
): number {
  return nodeMargin * Math.max(
    1,
    Math.min(leftRadius, rightRadius) / GRAPH_NODE_GAP_REFERENCE_RADIUS,
  );
}

/**
 * Expand and centre the converged cloud before enforcing its pixel-space
 * footprints. Normalised coordinates receive one scale, so topology, stress,
 * and crossings are unchanged while the renderer spends the available canvas.
 */
function fitGraphToViewport(
  data: ShapedGraphData,
  viewport: GraphFocusViewport,
): ShapedGraphData {
  if (data.nodes.length < 2) return data;
  const padding = Math.max(0, viewport.padding ?? GRAPH_ENGINE_STAGE_PADDING);
  const spanX = Math.max(1, viewport.width - padding * 2);
  const spanY = Math.max(1, viewport.height - padding * 2);
  const maximumFootprint = Math.max(...data.nodes.map(graphNodeFootprintRadius)) +
    GRAPH_FOCUS_RING_CLEARANCE;
  const availableX = Math.max(0, 2 - (maximumFootprint * 4) / spanX);
  const availableY = Math.max(0, 2 - (maximumFootprint * 4) / spanY);
  const minimumX = Math.min(...data.nodes.map((node) => node.x));
  const maximumX = Math.max(...data.nodes.map((node) => node.x));
  const minimumY = Math.min(...data.nodes.map((node) => node.y));
  const maximumY = Math.max(...data.nodes.map((node) => node.y));
  const width = Math.max(1e-12, maximumX - minimumX);
  const height = Math.max(1e-12, maximumY - minimumY);
  const scale = Math.min(availableX / width, availableY / height);
  const centreX = (minimumX + maximumX) / 2;
  const centreY = (minimumY + maximumY) / 2;

  return {
    nodes: data.nodes.map((node) => ({
      ...node,
      x: (node.x - centreX) * scale,
      y: (node.y - centreY) * scale,
    })),
    edges: data.edges,
  };
}

function removeGraphNodeOverlaps(
  data: ShapedGraphData,
  viewport: GraphFocusViewport,
  nodeMargin: number,
): ShapedGraphData {
  if (data.nodes.length < 2) return data;
  const padding = Math.max(0, viewport.padding ?? GRAPH_ENGINE_STAGE_PADDING);
  const spanX = Math.max(1, viewport.width - padding * 2);
  const spanY = Math.max(1, viewport.height - padding * 2);
  const x = Float64Array.from(data.nodes, (node) => (node.x * spanX) / 2);
  const y = Float64Array.from(data.nodes, (node) => (node.y * spanY) / 2);
  const footprints = Float64Array.from(data.nodes, graphNodeFootprintRadius);
  const clampToCanvas = (index: number): void => {
    const inset = footprints[index]! + GRAPH_FOCUS_RING_CLEARANCE;
    x[index] = Math.max(
      -viewport.width / 2 + inset,
      Math.min(viewport.width / 2 - inset, x[index]!),
    );
    y[index] = Math.max(
      -viewport.height / 2 + inset,
      Math.min(viewport.height / 2 - inset, y[index]!),
    );
  };

  const placeOnScanlineGrid = (): void => {
    const maximumRadius = Math.max(...footprints);
    const maximumNodeRadius = Math.max(...data.nodes.map((node) => node.size));
    const spacing = maximumRadius * 2 +
      graphNodeGap(maximumNodeRadius, maximumNodeRadius, nodeMargin);
    const inset = maximumRadius + GRAPH_FOCUS_RING_CLEARANCE;
    const columns = Math.max(1, Math.floor((viewport.width - inset * 2) / spacing) + 1);
    const rows = Math.max(1, Math.floor((viewport.height - inset * 2) / spacing) + 1);
    const slots = columns * rows;
    if (slots < data.nodes.length) return;
    const startX = -((columns - 1) * spacing) / 2;
    const startY = -((rows - 1) * spacing) / 2;
    const occupied = new Uint8Array(slots);
    const order = data.nodes
      .map((node, index) => ({ index, size: footprints[index]! }))
      .sort((left, right) => right.size - left.size || left.index - right.index);

    for (const node of order) {
      let nearest = -1;
      let nearestDistance = Number.POSITIVE_INFINITY;
      for (let slot = 0; slot < slots; slot += 1) {
        if (occupied[slot]) continue;
        const column = slot % columns;
        const row = Math.floor(slot / columns);
        const slotX = startX + column * spacing;
        const slotY = startY + row * spacing;
        const distance = Math.pow(slotX - x[node.index]!, 2) +
          Math.pow(slotY - y[node.index]!, 2);
        if (distance < nearestDistance) {
          nearest = slot;
          nearestDistance = distance;
        }
      }
      occupied[nearest] = 1;
      x[node.index] = startX + (nearest % columns) * spacing;
      y[node.index] = startY + Math.floor(nearest / columns) * spacing;
    }
  };

  if (data.nodes.length > 200) {
    placeOnScanlineGrid();
  } else {
    for (let iteration = 0; iteration < 64; iteration += 1) {
      let moved = false;
      for (let left = 0; left < data.nodes.length; left += 1) {
        for (let right = left + 1; right < data.nodes.length; right += 1) {
          let dx = x[left]! - x[right]!;
          let dy = y[left]! - y[right]!;
          let distance = Math.hypot(dx, dy);
          const required = footprints[left]! + footprints[right]! + graphNodeGap(
            data.nodes[left]!.size,
            data.nodes[right]!.size,
            nodeMargin,
          );
          if (distance + 1e-6 >= required) continue;
          if (distance < 1e-9) {
            const angle = stableUnit(0, `${left}:${right}`) * Math.PI * 2;
            dx = Math.cos(angle);
            dy = Math.sin(angle);
            distance = 1;
          }
          const movement = (required - distance + 1e-6) / (2 * distance);
          x[left] = x[left]! + dx * movement;
          y[left] = y[left]! + dy * movement;
          x[right] = x[right]! - dx * movement;
          y[right] = y[right]! - dy * movement;
          clampToCanvas(left);
          clampToCanvas(right);
          moved = true;
        }
      }
      if (!moved) break;
    }

    let overlaps = false;
    for (let left = 0; left < data.nodes.length; left += 1) {
      for (let right = left + 1; right < data.nodes.length; right += 1) {
        const distance = Math.hypot(x[left]! - x[right]!, y[left]! - y[right]!);
        const required = footprints[left]! + footprints[right]! + graphNodeGap(
          data.nodes[left]!.size,
          data.nodes[right]!.size,
          nodeMargin,
        );
        if (distance + 1e-6 >= required) continue;
        overlaps = true;
        break;
      }
      if (overlaps) break;
    }
    if (overlaps) placeOnScanlineGrid();
  }

  return {
    nodes: data.nodes.map((node, index) => ({
      ...node,
      x: (x[index]! * 2) / spanX,
      y: (y[index]! * 2) / spanY,
    })),
    edges: data.edges,
  };
}

/**
 * Sparse stress minimisation by stochastic gradient descent. Max-min pivots
 * provide O(kN) target pairs, while the centrality forest supplies the stable
 * topology-aware starting position.
 */
export function layoutGraphBySparseStress(
  data: ShapedGraphData,
  options: GraphSparseStressLayoutOptions = {},
): GraphSparseStressLayout {
  if (data.nodes.length < 2 || data.edges.length === 0) {
    return {
      data,
      pivots: data.nodes.map((node) => node.id),
      epochs: 0,
      pairCount: 0,
      parentByNode: Object.fromEntries(data.nodes.map((node) => [node.id, null])),
      childrenByNode: Object.fromEntries(data.nodes.map((node) => [node.id, []])),
    };
  }
  const seed = Math.trunc(options.seed ?? 0);
  const epochs = Math.max(1, Math.trunc(options.epochs ?? 24));
  const pivotCount = Math.max(
    1,
    Math.min(
      data.nodes.length,
      Math.trunc(options.pivotCount ?? Math.min(32, Math.ceil(Math.sqrt(data.nodes.length)))),
    ),
  );
  const random = seededRandom(seed);
  const { pairs, pivots } = sparseStressPairs(data, pivotCount, random);
  const initialLayout = layoutGraphByCentrality(data, { seed, hubCount: options.hubCount });
  const initial = initialLayout.data;
  if (options.viewport && pairs.length > 0) {
    const padding = Math.max(
      0,
      options.viewport.padding ?? GRAPH_ENGINE_STAGE_PADDING,
    );
    const spanX = Math.max(1, options.viewport.width - padding * 2);
    const spanY = Math.max(1, options.viewport.height - padding * 2);
    const nodeMargin = Math.max(0, finite(options.nodeMargin, 4));
    let pixelsPerTarget = 0;
    for (const pair of pairs) {
      const left = initial.nodes[pair.left]!;
      const right = initial.nodes[pair.right]!;
      pixelsPerTarget += Math.hypot(
        ((left.x - right.x) / 2) * spanX,
        ((left.y - right.y) / 2) * spanY,
      ) / pair.target;
    }
    pixelsPerTarget = Math.max(1e-6, pixelsPerTarget / pairs.length);
    for (const pair of pairs) {
      const left = data.nodes[pair.left]!;
      const right = data.nodes[pair.right]!;
      const radiusTarget = (
        graphNodeFootprintRadius(left) +
        graphNodeFootprintRadius(right) +
        graphNodeGap(left.size, right.size, nodeMargin)
      ) / pixelsPerTarget;
      pair.target = Math.max(pair.target, radiusTarget);
    }
  }
  data = initial;
  const x = Float64Array.from(initial.nodes, (node) => node.x);
  const y = Float64Array.from(initial.nodes, (node) => node.y);
  let maximumTarget = 1;
  for (const pair of pairs) maximumTarget = Math.max(maximumTarget, pair.target);
  const etaMaximum = maximumTarget * maximumTarget;
  const etaMinimum = 0.01;

  for (let epoch = 0; epoch < epochs; epoch += 1) {
    for (let index = pairs.length - 1; index > 0; index -= 1) {
      const swap = Math.floor(random() * (index + 1));
      [pairs[index], pairs[swap]] = [pairs[swap]!, pairs[index]!];
    }
    const progress = epochs === 1 ? 1 : epoch / (epochs - 1);
    const eta = etaMaximum * Math.pow(etaMinimum / etaMaximum, progress);
    for (const pair of pairs) {
      let dx = x[pair.left]! - x[pair.right]!;
      let dy = y[pair.left]! - y[pair.right]!;
      let distance = Math.hypot(dx, dy);
      if (distance < 1e-9) {
        const angle = stableUnit(seed, `${pair.left}:${pair.right}`) * Math.PI * 2;
        dx = Math.cos(angle) * 1e-6;
        dy = Math.sin(angle) * 1e-6;
        distance = 1e-6;
      }
      const step = Math.min(1, eta * pair.weight);
      const movement = (step * (distance - pair.target)) / (2 * distance);
      const moveX = dx * movement;
      const moveY = dy * movement;
      x[pair.left] = x[pair.left]! - moveX;
      y[pair.left] = y[pair.left]! - moveY;
      x[pair.right] = x[pair.right]! + moveX;
      y[pair.right] = y[pair.right]! + moveY;
    }
  }

  let result = positionedGraph(data, x, y);
  if (data.nodes.length <= 200 && data.edges.length <= 400) {
    const crossingLimit = countGraphEdgeCrossings(initial);
    if (countGraphEdgeCrossings(result) > crossingLimit) {
      let best = initial;
      let bestStress = graphNormalizedStress(initial);
      for (const progress of [0.875, 0.75, 0.625, 0.5, 0.375, 0.25, 0.125]) {
        const interpolated = interpolateGraphs(data, initial, result, progress);
        if (countGraphEdgeCrossings(interpolated) > crossingLimit) continue;
        const stress = graphNormalizedStress(interpolated);
        if (stress < bestStress) {
          best = interpolated;
          bestStress = stress;
        }
      }
      result = best;
    }
  }
  if (options.viewport) {
    result = fitGraphToViewport(result, options.viewport);
    result = removeGraphNodeOverlaps(
      result,
      options.viewport,
      Math.max(0, finite(options.nodeMargin, 4)),
    );
  }
  return {
    data: result,
    pivots: pivots.map((index) => data.nodes[index]!.id),
    epochs,
    pairCount: pairs.length,
    parentByNode: initialLayout.parentByNode,
    childrenByNode: initialLayout.childrenByNode,
  };
}

/** Derive graph density from usable canvas area and the nodes' rendered radii. */
export function deriveGraphLevelOfDetail(
  data: ShapedGraphData,
  viewport: GraphFocusViewport,
): GraphLevelOfDetailProfile {
  const padding = Math.max(0, viewport.padding ?? GRAPH_ENGINE_STAGE_PADDING);
  const area = Math.max(1, viewport.width - padding * 2) *
    Math.max(1, viewport.height - padding * 2);
  const averageRadius = data.nodes.length === 0
    ? 8
    : data.nodes.reduce((total, node) => total + node.size, 0) / data.nodes.length;
  const fullSizeCapacity = Math.max(
    1,
    Math.floor((area * 0.68) / (Math.PI * Math.pow(averageRadius + 2, 2))),
  );
  const nodeScale = rounded(Math.max(
    0.35,
    Math.min(1, Math.sqrt(fullSizeCapacity / Math.max(1, data.nodes.length))),
  ));
  const renderedRadius = averageRadius * nodeScale;
  const nodeLimit = Math.max(
    1,
    Math.floor((area * 0.68) / (Math.PI * Math.pow(renderedRadius + 2, 2))),
  );
  return {
    nodeLimit,
    nodeScale,
    radiusStep: rounded(Math.max(1, nodeScale * 2)),
    labelLimit: Math.max(
      1,
      Math.floor((48 * nodeScale) / Math.sqrt(Math.max(1, data.nodes.length / 100))),
    ),
  };
}

/**
 * Apply a canvas-derived level of detail to positioned data. The deepest
 * complete frontier that fits stays visible; descendants become expandable
 * summary nodes without changing the positions owned by sparse-stress layout.
 */
export function applyGraphLevelOfDetail(
  data: ShapedGraphData,
  parentByNode: Readonly<Record<string, string | null>>,
  options: GraphLevelOfDetailOptions,
): GraphLevelOfDetail {
  const profile = deriveGraphLevelOfDetail(data, options);
  const children = new Map(data.nodes.map((node) => [node.id, [] as string[]]));
  const roots: string[] = [];
  for (const node of data.nodes) {
    const parent = parentByNode[node.id];
    if (parent && children.has(parent)) children.get(parent)!.push(node.id);
    else roots.push(node.id);
  }

  const depth = new Map<string, number>();
  const order = [...roots];
  for (const root of roots) depth.set(root, 0);
  for (let index = 0; index < order.length; index += 1) {
    const id = order[index]!;
    for (const child of children.get(id) ?? []) {
      depth.set(child, depth.get(id)! + 1);
      order.push(child);
    }
  }
  const countByDepth = new Map<number, number>();
  for (const value of depth.values()) countByDepth.set(value, (countByDepth.get(value) ?? 0) + 1);
  let collapseDepth = Number.POSITIVE_INFINITY;
  let visibleAtFrontier = 0;
  const maximumDepth = Math.max(0, ...depth.values());
  for (let level = 0; level <= maximumDepth; level += 1) {
    const next = visibleAtFrontier + (countByDepth.get(level) ?? 0);
    if (next > profile.nodeLimit) {
      collapseDepth = Math.max(0, level - 1);
      break;
    }
    visibleAtFrontier = next;
  }

  const expanded = new Set(options.expandedSubtrees ?? []);
  const replacementByNode = new Map<string, string>();
  const summaries: ShapedGraphNode[] = [];
  if (Number.isFinite(collapseDepth)) {
    for (const root of order) {
      if (depth.get(root) !== collapseDepth || children.get(root)!.length === 0 || expanded.has(root)) {
        continue;
      }
      const members: string[] = [];
      const pending = [root];
      while (pending.length > 0) {
        const member = pending.pop()!;
        members.push(member);
        pending.push(...(children.get(member) ?? []));
      }
      const source = data.nodes.find((node) => node.id === root)!;
      const id = `__graph-summary__:${encodeURIComponent(root)}`;
      for (const member of members) replacementByNode.set(member, id);
      summaries.push({
        ...source,
        id,
        label: `${source.label} subtree (${members.length})`,
        size: source.size + Math.ceil(Math.log2(members.length)) * profile.radiusStep,
        isTrunk: true,
        collapsedSubtree: root,
        memberIds: members,
      });
    }
  }

  const nodes = [
    ...data.nodes.filter((node) => !replacementByNode.has(node.id)),
    ...summaries,
  ];
  const restingLabels = new Set(
    nodes
      .filter((node) => node.isTrunk || node.collapsedSubtree)
      .sort((left, right) =>
        Number(Boolean(right.collapsedSubtree)) - Number(Boolean(left.collapsedSubtree)) ||
        (left.trunkRank ?? Number.POSITIVE_INFINITY) -
          (right.trunkRank ?? Number.POSITIVE_INFINITY) ||
        left.id.localeCompare(right.id),
      )
      .slice(0, profile.labelLimit)
      .map((node) => node.id),
  );
  const detailedNodes = nodes.map((node) => ({
    ...node,
    size: rounded(node.size * profile.nodeScale),
    ...(node.isTrunk ? { isTrunk: restingLabels.has(node.id) } : {}),
  }));

  const edgeKeys = new Set<string>();
  const edges: ShapedGraphEdge[] = [];
  for (const edge of data.edges) {
    const source = replacementByNode.get(edge.source) ?? edge.source;
    const target = replacementByNode.get(edge.target) ?? edge.target;
    if (source === target) continue;
    const key = source < target ? `${source}\u0000${target}` : `${target}\u0000${source}`;
    if (edgeKeys.has(key)) continue;
    edgeKeys.add(key);
    edges.push({ ...edge, source, target });
  }

  return {
    ...profile,
    sourceNodeCount: data.nodes.length,
    data: { nodes: detailedNodes, edges },
  };
}

/** Ratio of overlapping node pairs to visible nodes in a fixed viewport. */
export function countGraphNodeOverlapRatio(
  data: ShapedGraphData,
  viewport: GraphFocusViewport,
): number {
  if (data.nodes.length === 0) return 0;
  const padding = Math.max(0, viewport.padding ?? GRAPH_ENGINE_STAGE_PADDING);
  const spanX = Math.max(0, viewport.width - padding * 2);
  const spanY = Math.max(0, viewport.height - padding * 2);
  let overlaps = 0;
  for (let left = 0; left < data.nodes.length; left += 1) {
    const first = data.nodes[left]!;
    const firstX = ((first.x + 1) / 2) * spanX + padding;
    const firstY = ((first.y + 1) / 2) * spanY + padding;
    for (let right = left + 1; right < data.nodes.length; right += 1) {
      const second = data.nodes[right]!;
      const secondX = ((second.x + 1) / 2) * spanX + padding;
      const secondY = ((second.y + 1) / 2) * spanY + padding;
      if (Math.hypot(firstX - secondX, firstY - secondY) < first.size + second.size) {
        overlaps += 1;
      }
    }
  }
  return overlaps / data.nodes.length;
}

/** Scale-independent Kruskal stress over all reachable node pairs. */
export function graphNormalizedStress(data: ShapedGraphData): number {
  if (data.nodes.length < 2) return 0;
  const adjacency = indexedAdjacency(data);
  let squaredLayout = 0;
  let layoutTarget = 0;
  let squaredTarget = 0;
  for (let source = 0; source < data.nodes.length; source += 1) {
    const distances = breadthFirstDistances(adjacency, source);
    const left = data.nodes[source]!;
    for (let target = source + 1; target < data.nodes.length; target += 1) {
      const graphDistance = distances[target]!;
      if (graphDistance <= 0) continue;
      const right = data.nodes[target]!;
      const layoutDistance = Math.hypot(left.x - right.x, left.y - right.y);
      const weight = 1 / (graphDistance * graphDistance);
      squaredLayout += weight * layoutDistance * layoutDistance;
      layoutTarget += weight * layoutDistance * graphDistance;
      squaredTarget += weight * graphDistance * graphDistance;
    }
  }
  if (squaredTarget === 0) return 0;
  if (squaredLayout === 0) return 1;
  const residual = Math.max(0, squaredTarget - (layoutTarget * layoutTarget) / squaredLayout);
  return rounded(Math.sqrt(residual / squaredTarget));
}

/** Replace selected communities with one deterministic, member-sized node. */
export function collapseGraphCommunities(
  data: ShapedGraphData,
  collapsedCommunities: Iterable<string>,
): ShapedGraphData {
  const collapsed = new Set(collapsedCommunities);
  if (collapsed.size === 0) return data;

  const groups = groupGraphCommunities(data.nodes).filter((group) => collapsed.has(group.id));
  if (groups.length === 0) return data;

  const replacementByNode = new Map<string, string>();
  const supernodes = groups.map((group): ShapedGraphNode => {
    const id = `__graph-community__:${encodeURIComponent(group.id)}`;
    for (const node of group.nodes) replacementByNode.set(node.id, id);
    return {
      id,
      label: `${group.label} (${group.nodes.length})`,
      community: group.id,
      communityExplicit: true,
      color: group.color,
      size:
        GRAPH_SUPERNODE_BASE_SIZE +
        Math.sqrt(group.nodes.length) * GRAPH_SUPERNODE_MEMBER_SCALE,
      x: rounded(group.nodes.reduce((sum, node) => sum + node.x, 0) / group.nodes.length),
      y: rounded(group.nodes.reduce((sum, node) => sum + node.y, 0) / group.nodes.length),
      collapsedCommunity: group.id,
      memberIds: group.nodes.map((node) => node.id),
    };
  });

  const nodes = [
    ...data.nodes.filter((node) => !replacementByNode.has(node.id)),
    ...supernodes,
  ];
  const edges = data.edges.flatMap((edge): ShapedGraphEdge[] => {
    const source = replacementByNode.get(edge.source) ?? edge.source;
    const target = replacementByNode.get(edge.target) ?? edge.target;
    if (source === target) return [];
    return [{ ...edge, source, target }];
  });
  return { nodes, edges };
}

function samePoint(left: GraphPoint, right: GraphPoint): boolean {
  return left.x === right.x && left.y === right.y;
}

function straightEdgePaths(data: ShapedGraphData): GraphEdgePath[] {
  const nodes = new Map(data.nodes.map((node) => [node.id, node]));
  return data.edges.map((edge) => ({
    ...edge,
    points: [nodes.get(edge.source)!, nodes.get(edge.target)!],
  }));
}

function orientation(a: GraphPoint, b: GraphPoint, c: GraphPoint): number {
  return (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
}

function segmentsCross(a: GraphPoint, b: GraphPoint, c: GraphPoint, d: GraphPoint): boolean {
  if ([a, b].some((point) => samePoint(point, c) || samePoint(point, d))) return false;
  const first = orientation(a, b, c);
  const second = orientation(a, b, d);
  const third = orientation(c, d, a);
  const fourth = orientation(c, d, b);
  return first * second < 0 && third * fourth < 0;
}

function measurePathCrossings(edges: readonly GraphEdgePath[]): number {
  let crossings = 0;
  for (let left = 0; left < edges.length; left += 1) {
    for (let right = left + 1; right < edges.length; right += 1) {
      const a = edges[left]!;
      const b = edges[right]!;
      for (let ai = 1; ai < a.points.length; ai += 1) {
        for (let bi = 1; bi < b.points.length; bi += 1) {
          if (segmentsCross(a.points[ai - 1]!, a.points[ai]!, b.points[bi - 1]!, b.points[bi]!)) {
            crossings += 1;
          }
        }
      }
    }
  }

  return crossings;
}

/** Count straight-line edge crossings for a positioned graph. */
export function countGraphEdgeCrossings(data: ShapedGraphData): number {
  return measurePathCrossings(straightEdgePaths(data));
}

/** Group nodes without sorting or mutating them; first appearance determines palette order. */
export function groupGraphCommunities(
  nodes: readonly ShapedGraphNode[],
): readonly GraphCommunity[] {
  const groups = new Map<string, ShapedGraphNode[]>();
  for (const node of nodes) {
    const group = groups.get(node.community);
    if (group) group.push(node);
    else groups.set(node.community, [node]);
  }
  return [...groups].map(([id, members], index) => ({
    id,
    label: id === UNASSIGNED_COMMUNITY ? "Unassigned" : id,
    // The legend wears the colour its members are drawn in, so a caller's
    // explicit node colour and the shaped community colour both stay truthful.
    color: members[0]?.color ?? GRAPH_COLOR_TOKENS[index % GRAPH_COLOR_TOKENS.length]!,
    nodes: members,
  }));
}

/** Case-insensitive focus/search over the visible node contract. */
export function searchGraphNodes(
  nodes: readonly ShapedGraphNode[],
  query: string,
): readonly ShapedGraphNode[] {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return nodes;
  return nodes.filter((node) =>
    [node.id, node.label, node.community].some((value) =>
      value.toLocaleLowerCase().includes(needle),
    ),
  );
}

/** Derive focus emphasis without mutating the stable graph data. */
export function reduceGraphFocus(
  data: ShapedGraphData,
  focusedNode: string | null,
  pulse = 0,
  viewport?: GraphFocusViewport,
): FocusedGraphData {
  const pulseProgress = Math.min(1, Math.max(0, pulse));
  const neighbours = new Set<string>();
  if (focusedNode !== null) {
    for (const edge of data.edges) {
      if (edge.source === focusedNode) neighbours.add(edge.target);
      if (edge.target === focusedNode) neighbours.add(edge.source);
    }
  }

  const desiredScale = (node: ShapedGraphNode): number =>
    focusedNode === null
      ? 1
      : node.id === focusedNode
        ? GRAPH_FOCUSED_SCALE + pulseProgress * GRAPH_FOCUS_PULSE_SCALE
        : neighbours.has(node.id)
          ? GRAPH_NEIGHBOUR_SCALE
          : GRAPH_REST_SCALE;
  let growthProgress = 1;
  if (viewport && focusedNode !== null) {
    const padding = viewport.padding ?? GRAPH_ENGINE_STAGE_PADDING;
    const spanX = Math.max(0, viewport.width - padding * 2);
    const spanY = Math.max(0, viewport.height - padding * 2);
    const nodeMargin = Math.max(0, finite(viewport.nodeMargin, 4));
    const scaleAt = (node: ShapedGraphNode, progress: number): number => {
      const desired = desiredScale(node);
      return desired <= 1 ? desired : 1 + (desired - 1) * progress;
    };
    const retainsClearance = (progress: number): boolean => {
      for (let left = 0; left < data.nodes.length; left += 1) {
        const first = data.nodes[left]!;
        const firstRadius = first.size * scaleAt(first, progress);
        for (let right = left + 1; right < data.nodes.length; right += 1) {
          const second = data.nodes[right]!;
          const secondRadius = second.size * scaleAt(second, progress);
          const distance = Math.hypot(
            ((first.x - second.x) / 2) * spanX,
            ((first.y - second.y) / 2) * spanY,
          );
          if (
            distance + 1e-6 <
            firstRadius + secondRadius + graphNodeGap(firstRadius, secondRadius, nodeMargin)
          ) return false;
        }
      }
      return true;
    };
    if (!retainsClearance(1)) {
      let lower = 0;
      let upper = 1;
      for (let iteration = 0; iteration < 24; iteration += 1) {
        const candidate = (lower + upper) / 2;
        if (retainsClearance(candidate)) lower = candidate;
        else upper = candidate;
      }
      growthProgress = lower;
    }
  }
  const scale = (node: ShapedGraphNode): number => {
    const desired = desiredScale(node);
    return desired <= 1 ? desired : 1 + (desired - 1) * growthProgress;
  };

  return {
    nodes: data.nodes.map((node) => ({
      ...node,
      color:
        focusedNode !== null && node.id !== focusedNode && !neighbours.has(node.id)
          ? GRAPH_MUTED_COLOR_TOKEN
          : node.color,
      showLabel:
        node.isTrunk === true ||
        (focusedNode !== null &&
          (node.id === focusedNode ||
            (neighbours.size + 1 <= GRAPH_NEIGHBOUR_LABEL_LIMIT && neighbours.has(node.id)))),
      scale: scale(node),
    })),
    edges: data.edges.map((edge) => ({
      ...edge,
      color:
        focusedNode !== null && edge.source !== focusedNode && edge.target !== focusedNode
          ? GRAPH_MUTED_COLOR_TOKEN
          : edge.color,
      displayLabel:
        focusedNode !== null &&
        (edge.source === focusedNode || edge.target === focusedNode) &&
        edge.label !== undefined
          ? truncateEdgeLabel(edge.label)
          : undefined,
    })),
  };
}

/** Deterministic arrangement selected by the graph contract. */
export type GraphLayout = "stress" | "force" | "radial";
