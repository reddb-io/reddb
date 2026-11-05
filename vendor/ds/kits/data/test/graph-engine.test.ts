/**
 * @vitest-environment jsdom
 */

import Graphology from "graphology";
import { describe, expect, it, vi } from "vitest";
import { mountGraphEngine } from "../src/graph-engine";
import { layoutGraphBySparseStress, shapeGraphData } from "../src/graph";

const renderer = vi.hoisted(() => ({
  graph: undefined as Graphology | undefined,
}));

vi.mock("sigma", () => ({
  default: class SigmaStub {
    constructor(graph: Graphology) {
      renderer.graph = graph;
    }

    getCanvases() {
      return {};
    }

    on() {}
    refresh() {}
    kill() {}

    getCamera() {
      return { animate: vi.fn() };
    }
  },
}));

describe("graph engine arrangement", () => {
  it("preserves the SGD first-paint positions when the lazy engine mounts", () => {
    const firstPaint = layoutGraphBySparseStress(
      shapeGraphData({
        nodes: [{ id: "hub" }, { id: "left" }, { id: "right" }, { id: "leaf" }],
        edges: [
          { source: "hub", target: "left" },
          { source: "hub", target: "right" },
          { source: "left", target: "leaf" },
        ],
      }),
      { seed: 407 },
    ).data;

    const controller = mountGraphEngine(document.createElement("div"), firstPaint, undefined, {
      layout: "force",
    });
    const loadedPositions = firstPaint.nodes.map((node) => {
      const attributes = renderer.graph!.getNodeAttributes(node.id);
      return { id: node.id, x: Number(attributes.x), y: Number(attributes.y) };
    });

    expect(loadedPositions).toEqual(
      firstPaint.nodes.map(({ id, x, y }) => ({
        id,
        x: expect.closeTo(x, 6),
        y: expect.closeTo(y, 6),
      })),
    );
    controller.destroy();
  });
});
