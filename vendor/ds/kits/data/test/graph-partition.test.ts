import { describe, expect, it } from "vitest";
import { partitionGraphByCentrality, shapeGraphData } from "../src/graph";

function tree(nodeCount: number) {
  return shapeGraphData({
    nodes: Array.from({ length: nodeCount }, (_, index) => ({ id: `n${index}` })),
    edges: Array.from({ length: nodeCount - 1 }, (_, index) => ({
      source: `n${Math.floor(index / 2)}`,
      target: `n${index + 1}`,
    })),
  });
}

describe("centrality forest partition benchmark", () => {
  it.each([
    { nodes: 1_000, budgetMs: 250 },
    { nodes: 10_000, budgetMs: 1_500 },
  ])("partitions $nodes nodes within $budgetMs ms", ({ nodes, budgetMs }) => {
    const fixture = tree(nodes);
    const started = performance.now();
    const result = partitionGraphByCentrality(fixture, ["n0"], { seed: 407 });

    expect(Object.keys(result.parentByNode)).toHaveLength(nodes);
    expect(result.work.edgeVisits).toBe((nodes - 1) * 2);
    expect(result.work.queuePushes).toBeLessThanOrEqual(nodes * 2);
    expect(result.work.queuePops).toBe(result.work.queuePushes);
    expect(performance.now() - started).toBeLessThan(budgetMs);
  });

  it("keeps 10k partition work near-linear against the 1k baseline", () => {
    const small = partitionGraphByCentrality(tree(1_000), ["n0"], { seed: 407 }).work;
    const large = partitionGraphByCentrality(tree(10_000), ["n0"], { seed: 407 }).work;

    expect(large.edgeVisits / small.edgeVisits).toBeLessThan(10.1);
    expect(large.queuePushes / small.queuePushes).toBeLessThan(10.1);
  });
});
