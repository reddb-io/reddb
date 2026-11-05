import { describe, expect, it } from "vitest";
import {
  applyGraphLevelOfDetail,
  countGraphNodeOverlapRatio,
  deriveGraphLevelOfDetail,
  layoutGraphBySparseStress,
  shapeGraphData,
} from "../src/graph";

function tree(nodeCount: number) {
  return shapeGraphData({
    nodes: Array.from({ length: nodeCount }, (_, index) => ({ id: `n${index}` })),
    edges: Array.from({ length: nodeCount - 1 }, (_, index) => ({
      source: `n${Math.floor(index / 2)}`,
      target: `n${index + 1}`,
    })),
  });
}

describe("graph level of detail", () => {
  it("derives capacity from canvas area and count-responsive node geometry", () => {
    const data = tree(1_000);
    const compact = deriveGraphLevelOfDetail(data, { width: 640, height: 400, padding: 40 });
    const large = deriveGraphLevelOfDetail(data, { width: 1_280, height: 800, padding: 40 });
    const sparse = deriveGraphLevelOfDetail(tree(100), { width: 640, height: 400, padding: 40 });

    expect(large.nodeLimit).toBeGreaterThan(compact.nodeLimit);
    expect(compact.nodeScale).toBeLessThan(sparse.nodeScale);
    expect(compact.radiusStep).toBeLessThan(sparse.radiusStep);
    expect(compact.labelLimit).toBeLessThan(sparse.labelLimit);
  });

  it("collapses deep subtrees above the derived limit and expands a selected summary", () => {
    const layout = layoutGraphBySparseStress(tree(2_000), {
      seed: 407,
      pivotCount: 8,
      epochs: 1,
      hubCount: 1,
    });
    const detail = applyGraphLevelOfDetail(layout.data, layout.parentByNode, {
      width: 640,
      height: 400,
      padding: 40,
    });
    const summary = detail.data.nodes.find((node) => node.collapsedSubtree);

    expect(detail.sourceNodeCount).toBe(2_000);
    expect(detail.data.nodes.length).toBeLessThanOrEqual(detail.nodeLimit);
    expect(summary?.memberIds?.length).toBeGreaterThan(1);
    expect(summary?.label).toContain(`(${summary?.memberIds?.length})`);
    expect(new Set(detail.data.nodes.map((node) => `${node.x}:${node.y}`)).size)
      .toBe(detail.data.nodes.length);

    const expanded = applyGraphLevelOfDetail(layout.data, layout.parentByNode, {
      width: 640,
      height: 400,
      padding: 40,
      expandedSubtrees: [summary!.collapsedSubtree!],
    });

    expect(expanded.data.nodes.some((node) => node.collapsedSubtree === summary!.collapsedSubtree))
      .toBe(false);
    expect(expanded.data.nodes.length).toBeGreaterThan(detail.data.nodes.length);
  });

  it("keeps node overlap below 1% at 1k nodes in a fixed canvas", () => {
    const columns = 40;
    const rows = 25;
    const positioned = shapeGraphData({
      nodes: Array.from({ length: columns * rows }, (_, index) => ({
        id: `n${index}`,
        x: -0.98 + (1.96 * (index % columns)) / (columns - 1),
        y: -0.98 + (1.96 * Math.floor(index / columns)) / (rows - 1),
      })),
      edges: [],
    });
    const parentByNode = Object.fromEntries(positioned.nodes.map((node) => [node.id, null]));
    const detail = applyGraphLevelOfDetail(positioned, parentByNode, {
      width: 640,
      height: 400,
      padding: 40,
    });

    expect(countGraphNodeOverlapRatio(detail.data, { width: 640, height: 400, padding: 40 }))
      .toBeLessThan(0.01);
  });
});
