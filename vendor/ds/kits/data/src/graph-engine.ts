import Graphology from "graphology";
import Sigma from "sigma";
import {
  GRAPH_EDGE_COLOR_TOKEN,
  GRAPH_ENGINE_STAGE_PADDING,
  reduceGraphFocus,
  type FocusedGraphEdge,
  type FocusedGraphNode,
  type GraphColorToken,
  type ShapedGraphData,
} from "./graph";

export interface GraphEngineOptions {
  layout: "stress" | "force" | "radial";
}

export interface GraphEngineController {
  destroy(): void;
  focusNode(id: string | null): void;
  setData(data: ShapedGraphData, options?: GraphEngineOptions): void;
}

interface ResolvedColors {
  background: string;
  foreground: string;
  edge: string;
  token(token: GraphColorToken): string;
}

function cssTokenName(token: string): string | undefined {
  return /^var\((--[^)]+)\)$/.exec(token)?.[1];
}

function resolveColors(host: HTMLElement): ResolvedColors {
  const style = getComputedStyle(host);
  const resolve = (token: string, fallback: string): string => {
    const name = cssTokenName(token);
    return (name ? style.getPropertyValue(name).trim() : "") || fallback;
  };
  // Without the DS stylesheets the canvas falls back to the host's own
  // computed ink rather than to a colour literal the Theme never declared.
  const ink = style.color;
  return {
    background: resolve("var(--reddb-color-background)", "transparent"),
    foreground: resolve("var(--reddb-color-foreground)", ink),
    edge: resolve(GRAPH_EDGE_COLOR_TOKEN, ink),
    token: (token) => resolve(token, ink),
  };
}

function replaceGraphData(
  graph: Graphology,
  data: ShapedGraphData,
  colors: ResolvedColors,
): ShapedGraphData {
  graph.clear();
  for (const node of data.nodes) {
    graph.addNode(node.id, {
      label: node.label,
      x: node.x,
      y: node.y,
      size: node.size,
      color: colors.token(node.color),
      community: node.community,
    });
  }
  for (const edge of data.edges) {
    graph.addEdgeWithKey(edge.id, edge.source, edge.target, {
      label: edge.label,
      size: Math.max(1, edge.weight),
      weight: edge.weight,
      color: colors.token(edge.color),
    });
  }

  return {
    nodes: data.nodes.map((node) => {
      const attributes = graph.getNodeAttributes(node.id);
      return {
        ...node,
        x: Number(attributes.x),
        y: Number(attributes.y),
        community: String(attributes.community),
      };
    }),
    edges: data.edges,
  };
}

/** Create the canvas renderer. This module is reached only by Graph's on-mount import. */
export function mountGraphEngine(
  host: HTMLElement,
  initialData: ShapedGraphData,
  interact?: (kind: "node" | "edge", id: string) => void,
  _initialOptions: GraphEngineOptions = { layout: "stress" },
): GraphEngineController {
  const colors = resolveColors(host);
  const graph = new Graphology({ type: "undirected", multi: true });
  let data = initialData;
  let focusedNode: string | null = null;
  let focusedNodes = new Map<string, FocusedGraphNode>();
  let focusedEdges = new Map<string, FocusedGraphEdge>();
  let pulseFrame: number | undefined;
  let layout = replaceGraphData(graph, initialData, colors);
  data = layout;

  const renderer = new Sigma(graph, host, {
    defaultEdgeColor: colors.edge,
    defaultNodeColor: colors.foreground,
    renderEdgeLabels: true,
    labelColor: { color: colors.foreground },
    nodeReducer: (id, attributes) => {
      const node = focusedNodes.get(id);
      if (!node) return attributes;
      return {
        ...attributes,
        color: colors.token(node.color),
        forceLabel: node.id === focusedNode,
        label: node.showLabel ? node.label : "",
        size: node.size * node.scale,
      };
    },
    edgeReducer: (id, attributes) => {
      const edge = focusedEdges.get(id);
      if (!edge) return attributes;
      return {
        ...attributes,
        color: colors.token(edge.color),
        forceLabel: edge.displayLabel !== undefined,
        label: edge.displayLabel ?? "",
      };
    },
    stagePadding: GRAPH_ENGINE_STAGE_PADDING,
  });
  const canvases = renderer.getCanvases();
  if (canvases.edgeLabels && canvases.labels) {
    canvases.labels.before(canvases.edgeLabels);
  }

  const updateFocusState = (pulseProgress = 0): void => {
    const focus = reduceGraphFocus(data, focusedNode, pulseProgress);
    focusedNodes = new Map(focus.nodes.map((node) => [node.id, node]));
    focusedEdges = new Map(focus.edges.map((edge) => [edge.id, edge]));
  };

  const applyFocus = (id: string | null): void => {
    if (pulseFrame !== undefined) cancelAnimationFrame(pulseFrame);
    pulseFrame = undefined;
    focusedNode = id !== null && graph.hasNode(id) ? id : null;
    updateFocusState();
    renderer.refresh();
  };

  const pulse = (id: string): void => {
    if (globalThis.matchMedia?.("(prefers-reduced-motion: reduce)").matches) return;
    if (pulseFrame !== undefined) cancelAnimationFrame(pulseFrame);
    const started = performance.now();
    const frame = (now: number): void => {
      const progress = Math.min(1, (now - started) / 420);
      updateFocusState(Math.sin(progress * Math.PI));
      renderer.refresh();
      if (progress < 1 && focusedNode === id) pulseFrame = requestAnimationFrame(frame);
      else {
        pulseFrame = undefined;
        updateFocusState();
        renderer.refresh();
      }
    };
    pulseFrame = requestAnimationFrame(frame);
  };

  renderer.on("clickNode", ({ node }) => interact?.("node", node));
  renderer.on("clickEdge", ({ edge }) => interact?.("edge", edge));
  applyFocus(null);

  return {
    destroy: () => {
      if (pulseFrame !== undefined) cancelAnimationFrame(pulseFrame);
      renderer.kill();
    },
    focusNode: (id) => {
      applyFocus(id);
      if (!focusedNode) return;
      const { x, y } = graph.getNodeAttributes(focusedNode);
      renderer.getCamera().animate({ x, y, ratio: 0.35 }, { duration: 300 });
      pulse(focusedNode);
    },
    setData: (nextData) => {
      layout = replaceGraphData(graph, nextData, colors);
      data = layout;
      applyFocus(focusedNode);
    },
  };
}
