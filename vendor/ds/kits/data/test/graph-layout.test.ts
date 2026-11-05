import { describe, expect, it } from "vitest";
import {
  countGraphEdgeCrossings,
  graphNormalizedStress,
  layoutGraphByCentrality,
  layoutGraphBySparseStress,
  reduceGraphFocus,
  shapeGraphData,
  type GraphData,
  type ShapedGraphData,
} from "../src/graph";

function graph(
  nodeCount: number,
  edges: readonly (readonly [number, number])[],
  communities: (node: number) => string | undefined = () => undefined,
): ShapedGraphData {
  return shapeGraphData({
    nodes: Array.from({ length: nodeCount }, (_, index) => ({
      id: `n${index}`,
      ...(communities(index) === undefined ? {} : { community: communities(index) }),
    })),
    edges: edges.map(([source, target]) => ({ source: `n${source}`, target: `n${target}` })),
  });
}

function grid(side: number): ShapedGraphData {
  const edges: Array<readonly [number, number]> = [];
  for (let row = 0; row < side; row += 1) {
    for (let column = 0; column < side; column += 1) {
      const node = row * side + column;
      if (column + 1 < side) edges.push([node, node + 1]);
      if (row + 1 < side) edges.push([node, node + side]);
    }
  }
  return graph(side * side, edges);
}

function tree(nodeCount: number): ShapedGraphData {
  return graph(
    nodeCount,
    Array.from({ length: nodeCount - 1 }, (_, index) => [Math.floor(index / 2), index + 1] as const),
  );
}

function scaleFree(nodeCount: number): ShapedGraphData {
  const edges: Array<readonly [number, number]> = [[0, 1], [0, 2], [1, 2]];
  for (let node = 3; node < nodeCount; node += 1) {
    edges.push([node, 0], [node, 1]);
  }
  return graph(nodeCount, edges);
}

function denseScaleFree(nodeCount: number): ShapedGraphData {
  const fixture = scaleFree(nodeCount);
  const size = nodeCount >= 1_000 ? 2 : 8;
  return {
    ...fixture,
    nodes: fixture.nodes.map((node) => ({ ...node, size })),
  };
}

const densityMargins = {
  compact: 2,
  comfortable: 4,
  spacious: 8,
} as const;

const showcase = shapeGraphData({
  nodes: [
    { id: "gateway", community: "edge", size: 13 },
    { id: "cdn", community: "edge" },
    { id: "waf", community: "edge" },
    { id: "router", community: "edge" },
    { id: "firewall", community: "edge" },
    { id: "api", community: "services", size: 13 },
    { id: "worker", community: "services" },
    { id: "billing", community: "services" },
    { id: "search", community: "services" },
    { id: "auth", community: "services" },
    { id: "database", community: "storage", size: 13 },
    { id: "cache", community: "storage" },
    { id: "index", community: "storage" },
    { id: "archive", community: "storage" },
    { id: "replica", community: "storage" },
  ],
  edges: [
    { source: "gateway", target: "cdn", weight: 3 },
    { source: "gateway", target: "waf", weight: 3 },
    { source: "gateway", target: "router", weight: 2 },
    { source: "router", target: "firewall" },
    { source: "api", target: "worker", weight: 3 },
    { source: "api", target: "billing", weight: 3 },
    { source: "api", target: "search", weight: 2 },
    { source: "billing", target: "auth" },
    { source: "database", target: "cache", weight: 3 },
    { source: "database", target: "index", weight: 3 },
    { source: "database", target: "archive", weight: 2 },
    { source: "index", target: "replica" },
    { source: "gateway", target: "api", weight: 0.25 },
    { source: "api", target: "database", weight: 0.25 },
  ],
});

function overlappingPairs(
  data: ShapedGraphData,
  viewport: { width: number; height: number; padding: number },
  margin: number,
  scales: Readonly<Record<string, number>> = {},
): number {
  const spanX = viewport.width - viewport.padding * 2;
  const spanY = viewport.height - viewport.padding * 2;
  let overlaps = 0;
  for (let left = 0; left < data.nodes.length; left += 1) {
    const first = data.nodes[left]!;
    for (let right = left + 1; right < data.nodes.length; right += 1) {
      const second = data.nodes[right]!;
      const distance = Math.hypot(
        ((first.x - second.x) / 2) * spanX,
        ((first.y - second.y) / 2) * spanY,
      );
      const required =
        first.size * (scales[first.id] ?? 1) +
        second.size * (scales[second.id] ?? 1) +
        margin;
      if (distance + 1e-6 < required) overlaps += 1;
    }
  }
  return overlaps;
}

function collidingNodeCount(data: ShapedGraphData): number {
  const counts = new Map<string, number>();
  for (const node of data.nodes) {
    const position = `${node.x}:${node.y}`;
    counts.set(position, (counts.get(position) ?? 0) + 1);
  }
  return [...counts.values()].reduce(
    (total, count) => total + (count > 1 ? count : 0),
    0,
  );
}

function twoCommunity(size: number): ShapedGraphData {
  const edges: Array<readonly [number, number]> = [];
  for (let community = 0; community < 2; community += 1) {
    const start = community * size;
    for (let offset = 0; offset < size; offset += 1) {
      edges.push([start + offset, start + ((offset + 1) % size)]);
      edges.push([start + offset, start + ((offset + 3) % size)]);
    }
  }
  edges.push([0, size], [Math.floor(size / 2), size + Math.floor(size / 2)]);
  return graph(size * 2, edges, (node) => (node < size ? "alpha" : "beta"));
}

const fixtures = {
  grid: grid(5),
  tree: tree(31),
  "scale-free": scaleFree(36),
  "two-community": twoCommunity(14),
};

describe("sparse stochastic stress layout", () => {
  it.each(Object.entries(densityMargins))(
    "removes every showcase node overlap at the %s Density stop",
    (_density, nodeMargin) => {
      const viewport = { width: 1_012, height: 480, padding: 40 };
      const result = layoutGraphBySparseStress(showcase, {
        seed: 397,
        hubCount: 3,
        viewport,
        nodeMargin,
      });

      expect(overlappingPairs(result.data, viewport, nodeMargin)).toBe(0);
    },
  );

  it("preserves sparse-stress shape and the showcase trunk arrangement while removing overlaps", () => {
    const viewport = { width: 1_012, height: 480, padding: 40 };
    const baseline = layoutGraphBySparseStress(showcase, {
      seed: 397,
      hubCount: 3,
    }).data;
    const separated = layoutGraphBySparseStress(showcase, {
      seed: 397,
      hubCount: 3,
      viewport,
      nodeMargin: densityMargins.spacious,
    }).data;
    const orientation = (data: ShapedGraphData): number => {
      const nodes = new Map(data.nodes.map((node) => [node.id, node]));
      const gateway = nodes.get("gateway")!;
      const api = nodes.get("api")!;
      const database = nodes.get("database")!;
      return Math.sign(
        (api.x - gateway.x) * (database.y - gateway.y) -
          (api.y - gateway.y) * (database.x - gateway.x),
      );
    };

    expect(graphNormalizedStress(separated)).toBeLessThanOrEqual(
      graphNormalizedStress(baseline) * 1.05,
    );
    expect(orientation(separated)).toBe(orientation(baseline));
  });

  it.each(
    [100, 1_000].flatMap((nodes) =>
      Object.entries(densityMargins).map(([density, nodeMargin]) => ({
        nodes,
        density,
        nodeMargin,
      })),
    ),
  )(
    "removes every overlap from the $nodes-node dense fixture at the $density Density stop",
    ({ nodes, nodeMargin }) => {
      const viewport = { width: 1_012, height: 480, padding: 40 };
      const result = layoutGraphBySparseStress(denseScaleFree(nodes), {
        seed: 415,
        viewport,
        nodeMargin,
      });

      expect(overlappingPairs(result.data, viewport, nodeMargin)).toBe(0);
    },
    10_000,
  );

  it.each(Object.entries(densityMargins))(
    "keeps the focused showcase hub overlap-free at the %s Density stop",
    (_density, nodeMargin) => {
      const viewport = { width: 1_012, height: 480, padding: 40 };
      const layout = layoutGraphBySparseStress(showcase, {
        seed: 397,
        hubCount: 3,
        viewport,
        nodeMargin,
      });
      const focused = reduceGraphFocus(layout.data, "api", 0, { ...viewport, nodeMargin });
      const scales = Object.fromEntries(focused.nodes.map((node) => [node.id, node.scale]));

      expect(overlappingPairs(layout.data, viewport, nodeMargin, scales)).toBe(0);
    },
  );

  it.each([
    { nodes: 1_000, maxCollidingNodes: 0, budgetMs: 2_000 },
    { nodes: 10_000, maxCollidingNodes: 0, budgetMs: 12_000 },
  ])(
    "keeps coordinate collisions negligible at $nodes nodes",
    ({ nodes, maxCollidingNodes, budgetMs }) => {
      const fixture = scaleFree(nodes);
      const started = performance.now();
      const result = layoutGraphBySparseStress(fixture, { seed: 413 });

      expect(collidingNodeCount(result.data)).toBeLessThanOrEqual(maxCollidingNodes);
      expect(performance.now() - started).toBeLessThan(budgetMs);
    },
    10_000,
  );

  it.each(Object.entries(fixtures))(
    "materially lowers normalised stress for the %s fixture",
    (_name, fixture) => {
      const initial = layoutGraphByCentrality(fixture, { seed: 405 }).data;
      const result = layoutGraphBySparseStress(fixture, { seed: 405 });

      expect(graphNormalizedStress(result.data)).toBeLessThan(
        graphNormalizedStress(initial) * 0.9,
      );
    },
  );

  it.each(Object.entries(fixtures))(
    "does not regress edge crossings for the %s fixture",
    (_name, fixture) => {
      const initial = layoutGraphByCentrality(fixture, { seed: 405 }).data;
      const result = layoutGraphBySparseStress(fixture, { seed: 405 });

      expect(countGraphEdgeCrossings(result.data)).toBeLessThanOrEqual(
        countGraphEdgeCrossings(initial),
      );
    },
  );

  it("is identical for a fixed seed across repeated runs", () => {
    const first = layoutGraphBySparseStress(fixtures["scale-free"], { seed: 11 });
    const repeated = layoutGraphBySparseStress(fixtures["scale-free"], { seed: 11 });
    const different = layoutGraphBySparseStress(fixtures["scale-free"], { seed: 12 });

    expect(repeated).toEqual(first);
    expect(different.data.nodes).not.toEqual(first.data.nodes);
  });

  it.each([
    { nodes: 1_000, side: 40, budgetMs: 2_000 },
    { nodes: 10_000, side: 100, budgetMs: 12_000 },
  ])(
    "lays out $nodes nodes within the stated $budgetMs ms budget",
    ({ nodes, side, budgetMs }) => {
      const fixture = grid(side);
      const data: GraphData = {
        nodes: fixture.nodes.slice(0, nodes),
        edges: fixture.edges.filter((edge) => {
          const source = Number(edge.source.slice(1));
          const target = Number(edge.target.slice(1));
          return source < nodes && target < nodes;
        }),
      };
      const started = performance.now();
      const result = layoutGraphBySparseStress(shapeGraphData(data), {
        seed: 405,
        pivotCount: 16,
        epochs: 8,
      });

      expect(result.data.nodes).toHaveLength(nodes);
      expect(performance.now() - started).toBeLessThan(budgetMs);
    },
    20_000,
  );
});
