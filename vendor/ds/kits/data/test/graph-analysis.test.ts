import { describe, expect, it } from "vitest";
import {
  analyseGraphStructure,
  detectGraphCommunities,
  filterGraphBackbone,
  layoutGraphByCentrality,
  layoutGraphBySparseStress,
  rankGraphByBetweenness,
  reduceGraphFocus,
  shapeGraphData,
} from "../src/graph";

function starGraph(hubs: number, leavesPerHub: number) {
  const nodes = Array.from({ length: hubs }, (_, index) => ({ id: `hub-${index}` }));
  const edges: Array<{ source: string; target: string; weight?: number }> = [];
  for (let hub = 0; hub < hubs; hub += 1) {
    for (let leaf = 0; leaf < leavesPerHub; leaf += 1) {
      const id = `leaf-${hub}-${leaf}`;
      nodes.push({ id });
      edges.push({ source: `hub-${hub}`, target: id });
    }
    if (hub > 0) {
      edges.push({ source: `hub-${hub - 1}`, target: `hub-${hub}`, weight: 0.2 });
    }
  }
  return shapeGraphData({ nodes, edges });
}

function scaleFreeGraph(nodeCount: number) {
  const nodes = Array.from({ length: nodeCount }, (_, index) => ({ id: `scale-${index}` }));
  const edges: Array<{ source: string; target: string }> = [
    { source: "scale-0", target: "scale-1" },
    { source: "scale-1", target: "scale-2" },
    { source: "scale-2", target: "scale-0" },
  ];
  const degreeUrn = [0, 0, 1, 1, 2, 2];
  let state = 406;
  const randomIndex = (limit: number): number => {
    state = (Math.imul(state, 1_664_525) + 1_013_904_223) >>> 0;
    return state % limit;
  };
  for (let node = 3; node < nodeCount; node += 1) {
    const targets = new Set<number>();
    while (targets.size < 2) targets.add(degreeUrn[randomIndex(degreeUrn.length)]!);
    for (const target of targets) {
      edges.push({ source: `scale-${node}`, target: `scale-${target}` });
      degreeUrn.push(target, node);
    }
  }
  return shapeGraphData({ nodes, edges });
}

function plantedCommunities() {
  const nodes = Array.from({ length: 12 }, (_, index) => ({ id: `n${index}` }));
  const edges: Array<{ source: string; target: string }> = [];
  for (const start of [0, 6]) {
    for (let left = start; left < start + 6; left += 1) {
      for (let right = left + 1; right < start + 6; right += 1) {
        edges.push({ source: `n${left}`, target: `n${right}` });
      }
    }
  }
  edges.push({ source: "n5", target: "n6" });
  return shapeGraphData({ nodes, edges });
}

function isInternallyConnected(
  members: readonly string[],
  edges: readonly { source: string; target: string }[],
): boolean {
  if (members.length < 2) return true;
  const memberSet = new Set(members);
  const seen = new Set([members[0]!]);
  const pending = [members[0]!];
  while (pending.length > 0) {
    const current = pending.pop()!;
    for (const edge of edges) {
      const neighbour = edge.source === current
        ? edge.target
        : edge.target === current
          ? edge.source
          : undefined;
      if (neighbour && memberSet.has(neighbour) && !seen.has(neighbour)) {
        seen.add(neighbour);
        pending.push(neighbour);
      }
    }
  }
  return seen.size === members.length;
}

describe("graph structural analysis", () => {
  it("partitions a planted fixture with Leiden into internally connected communities", () => {
    const graph = plantedCommunities();
    const detected = detectGraphCommunities(graph, { seed: 406 });
    const groups = Map.groupBy(detected.nodes, (node) => node.community);
    const members = [...groups.values()]
      .map((nodes) => nodes.map((node) => node.id).sort())
      .sort((left, right) => left[0]!.localeCompare(right[0]!));

    expect(members).toEqual([
      ["n0", "n1", "n2", "n3", "n4", "n5"],
      ["n10", "n11", "n6", "n7", "n8", "n9"],
    ]);
    expect(members.every((community) => isInternallyConnected(community, graph.edges))).toBe(true);
  });

  it("keeps planted trunks and discards noise with the disparity filter at alpha 0.05", () => {
    const ids = ["a", "b", "c", "d", "e", "f"];
    const planted = new Set(["a-b", "c-d"]);
    const graph = shapeGraphData({
      nodes: ids.map((id) => ({ id })),
      edges: ids.flatMap((source, sourceIndex) =>
        ids.slice(sourceIndex + 1).map((target) => {
          const id = `${source}-${target}`;
          return { id, source, target, weight: planted.has(id) ? 20 : 1 };
        }),
      ),
    });

    const backbone = filterGraphBackbone(graph, { alpha: 0.05 });

    expect(backbone.edges.map((edge) => edge.id)).toEqual(["a-b", "c-d"]);
  });

  it("sampled betweenness ranks planted bridge nodes above equally-connected peers", () => {
    const left = ["bridge-left", "left-a", "left-b", "left-c", "peer-left"];
    const right = ["bridge-right", "right-a", "right-b", "right-c", "peer-right"];
    const cliqueEdges = (ids: readonly string[], bridge: string) => ids.flatMap((source, index) =>
      ids.slice(index + 1)
        .filter((target) => source !== bridge || target !== ids.at(-1))
        .map((target) => ({ source, target })),
    );
    const graph = shapeGraphData({
      nodes: [...left, "peer-left-leaf", ...right, "peer-right-leaf"].map((id) => ({ id })),
      edges: [
        ...cliqueEdges(left, "bridge-left"),
        ...cliqueEdges(right, "bridge-right"),
        { source: "peer-left", target: "peer-left-leaf" },
        { source: "peer-right", target: "peer-right-leaf" },
        { source: "bridge-left", target: "bridge-right" },
      ],
    });

    const scores = new Map(
      rankGraphByBetweenness(graph, { sampleSize: 8, seed: 406 })
        .map((entry) => [entry.id, entry.score]),
    );

    expect(graph.edges.filter((edge) => edge.source === "bridge-left" || edge.target === "bridge-left")).toHaveLength(4);
    expect(graph.edges.filter((edge) => edge.source === "peer-left" || edge.target === "peer-left")).toHaveLength(4);
    expect(scores.get("bridge-left")).toBeGreaterThan(scores.get("peer-left")!);
    expect(scores.get("bridge-right")).toBeGreaterThan(scores.get("peer-right")!);
  });

  it("derives trunk count from graph structure instead of a fixed cap", () => {
    const multiHub = analyseGraphStructure(starGraph(6, 8), { alpha: 0.05, seed: 406 });
    const singleHub = analyseGraphStructure(starGraph(1, 30), { alpha: 0.05, seed: 406 });
    const scaleFree = analyseGraphStructure(scaleFreeGraph(1_000), {
      alpha: 0.51,
      sampleSize: 40,
      seed: 406,
    });

    expect(multiHub.trunks.length).toBeGreaterThan(1);
    expect(singleHub.trunks).toEqual(["hub-0"]);
    expect(scaleFree.trunks.length).toBeGreaterThan(4);
  }, 10_000);

  it("feeds detected communities and uncapped trunks into automatic layout", () => {
    const layout = layoutGraphByCentrality(starGraph(6, 8), { seed: 406 });

    expect(layout.hubs).toHaveLength(6);
    expect(new Set(layout.data.nodes.map((node) => node.community)).size).toBe(6);
    expect(layout.hubs.every((id) => layout.data.nodes.find((node) => node.id === id)?.isTrunk)).toBe(true);
    expect(layout.data.nodes.filter((node) => node.isTrunk).every((node) => node.size > 8)).toBe(true);
  });

  it("carries trunk size, tint, and resting label priority through the default layout", () => {
    const layout = layoutGraphBySparseStress(starGraph(6, 8), { epochs: 1, seed: 406 });
    const visible = reduceGraphFocus(layout.data, null);
    const trunks = visible.nodes.filter((node) => node.isTrunk);

    expect(trunks).toHaveLength(6);
    expect(new Set(trunks.map((node) => node.color)).size).toBeGreaterThan(1);
    expect(trunks.every((node) => node.size > 8 && node.showLabel)).toBe(true);
    expect(visible.nodes.filter((node) => node.showLabel).map((node) => node.id)).toEqual(
      trunks.map((node) => node.id),
    );
  });
});
