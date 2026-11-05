import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import Graph from "../src/Graph.svelte";
import {
  GRAPH_COLOR_TOKENS,
  GRAPH_EDGE_LABEL_MAX_LENGTH,
  GRAPH_FOCUS_PULSE_SCALE,
  GRAPH_FOCUSED_SCALE,
  GRAPH_ENGINE_STAGE_PADDING,
  GRAPH_LAYOUT_GRAVITY,
  GRAPH_LAYOUT_SCALING_RATIO,
  GRAPH_LAYOUT_ITERATIONS_PER_NODE,
  GRAPH_LAYOUT_MAX_ITERATIONS,
  GRAPH_LAYOUT_MIN_AXIS_FILL,
  GRAPH_LAYOUT_MIN_ITERATIONS,
  GRAPH_MUTED_COLOR_TOKEN,
  GRAPH_NEIGHBOUR_LABEL_LIMIT,
  GRAPH_NEIGHBOUR_SCALE,
  GRAPH_REST_SCALE,
  collapseGraphCommunities,
  countGraphEdgeCrossings,
  groupGraphCommunities,
  layoutGraphByCentrality,
  reduceGraphFocus,
  searchGraphNodes,
  shapeGraphData,
} from "../src/graph";

const engine = vi.hoisted(() => ({
  loaded: false,
  mounted: 0,
  destroy: vi.fn(),
  focusNode: vi.fn(),
}));

vi.mock("../src/graph-engine", () => {
  engine.loaded = true;
  return {
    mountGraphEngine: () => {
      engine.mounted += 1;
      return { destroy: engine.destroy, focusNode: engine.focusNode, setData: vi.fn() };
    },
  };
});

const mounted: Array<{ instance: Record<string, unknown>; target: HTMLElement }> = [];

function render(props: Record<string, unknown>): HTMLElement {
  const target = document.createElement("div");
  document.body.append(target);
  const instance = mount(Graph, { target, props }) as Record<string, unknown>;
  mounted.push({ instance, target });
  flushSync();
  return target;
}

afterEach(() => {
  for (const entry of mounted.splice(0)) {
    unmount(entry.instance);
    entry.target.remove();
  }
  vi.unstubAllGlobals();
});

const data = {
  nodes: [
    { id: "api", label: "API", community: "services" },
    { id: "db", label: "Database", community: "storage" },
    { id: "cache", label: "Cache", community: "storage" },
  ],
  edges: [
    { source: "api", target: "db", weight: 2 },
    { source: "api", target: "cache" },
  ],
} as const;

const showcaseData = {
  nodes: [
    { id: "gateway", label: "Gateway", community: "edge", size: 13 },
    { id: "cdn", label: "CDN", community: "edge" },
    { id: "waf", label: "WAF", community: "edge" },
    { id: "router", label: "Router", community: "edge" },
    { id: "firewall", label: "Firewall", community: "edge" },
    { id: "api", label: "API", community: "services", size: 13 },
    { id: "worker", label: "Worker", community: "services" },
    { id: "billing", label: "Billing", community: "services" },
    { id: "search", label: "Search", community: "services" },
    { id: "auth", label: "Auth", community: "services" },
    { id: "database", label: "Database", community: "storage", size: 13 },
    { id: "cache", label: "Cache", community: "storage" },
    { id: "index", label: "Index", community: "storage" },
    { id: "archive", label: "Archive", community: "storage" },
    { id: "replica", label: "Replica", community: "storage" },
  ],
  edges: [
    ["gateway", "cdn", 3], ["gateway", "waf", 3], ["gateway", "router", 2],
    ["router", "firewall", 1], ["api", "worker", 3], ["api", "billing", 3],
    ["api", "search", 2], ["billing", "auth", 1], ["database", "cache", 3],
    ["database", "index", 3], ["database", "archive", 2], ["index", "replica", 1],
    ["gateway", "api", 0.25], ["api", "database", 0.25],
  ].map(([source, target, weight]) => ({ source: String(source), target: String(target), weight: Number(weight) })),
};

interface RenderedCircle {
  id: string;
  x: number;
  y: number;
  radius: number;
}

function renderedCircles(root: HTMLElement): RenderedCircle[] {
  return [...root.querySelectorAll<SVGCircleElement>("circle[data-node-id]")].map((circle) => ({
    id: circle.dataset.nodeId!,
    x: Number(circle.getAttribute("cx")),
    y: Number(circle.getAttribute("cy")),
    radius: Number(circle.getAttribute("r")),
  }));
}

function minimumGapRatio(density: "tiny" | "compact" | "comfortable" | "spacious"): number {
  return density === "tiny" ? 0.25 : density === "compact" ? 0.375 : density === "spacious" ? 1 : 0.5;
}

const islandData = shapeGraphData({
  nodes: [
    { id: "a1", community: "alpha", x: -0.9, y: -0.8 },
    { id: "a2", community: "alpha", x: -0.7, y: 0.8 },
    { id: "b1", community: "beta", x: 0.7, y: -0.8 },
    { id: "b2", community: "beta", x: 0.9, y: 0.8 },
  ],
  edges: [
    { id: "inside-alpha", source: "a1", target: "a2" },
    { id: "cross-1", source: "a1", target: "b2" },
    { id: "cross-2", source: "a2", target: "b1" },
  ],
});

const multiHubData = shapeGraphData({
  nodes: [
    { id: "hub-a", x: -0.95, y: 0 },
    { id: "hub-b", x: 0.95, y: 0 },
    { id: "hub-c", x: 0, y: 0.95 },
    { id: "a-1", x: 0.72, y: 0.52 },
    { id: "b-1", x: -0.72, y: -0.52 },
    { id: "c-1", x: 0.72, y: -0.52 },
    { id: "a-2", x: -0.72, y: 0.52 },
    { id: "b-2", x: 0, y: -0.95 },
    { id: "c-2", x: 0.2, y: 0.1 },
    { id: "a-leaf", x: -0.2, y: -0.1 },
    { id: "b-leaf", x: 0.45, y: 0.2 },
    { id: "c-leaf", x: -0.45, y: -0.2 },
  ],
  edges: [
    { source: "hub-a", target: "a-1", weight: 3 },
    { source: "hub-a", target: "a-2", weight: 2 },
    { source: "a-1", target: "a-leaf" },
    { source: "hub-b", target: "b-1", weight: 3 },
    { source: "hub-b", target: "b-2", weight: 2 },
    { source: "b-1", target: "b-leaf" },
    { source: "hub-c", target: "c-1", weight: 3 },
    { source: "hub-c", target: "c-2", weight: 2 },
    { source: "c-1", target: "c-leaf" },
    { source: "hub-a", target: "hub-b", weight: 0.25 },
    { source: "hub-b", target: "hub-c", weight: 0.25 },
  ],
});

describe("graph data shaping", () => {
  it("creates immutable engine-ready nodes and stable edge identities", () => {
    const shaped = shapeGraphData(data);

    expect(shaped.nodes.map(({ id, label, x, y }) => ({ id, label, x, y }))).toEqual([
      { id: "api", label: "API", x: 1, y: 0 },
      { id: "db", label: "Database", x: -0.5, y: 0.866025 },
      { id: "cache", label: "Cache", x: -0.5, y: -0.866025 },
    ]);
    expect(shaped.edges).toMatchObject([
      { id: "api→db:0", source: "api", target: "db", weight: 2 },
      { id: "api→cache:1", source: "api", target: "cache", weight: 1 },
    ]);
    expect(shaped.nodes[0]).not.toBe(data.nodes[0]);
    expect(data.nodes[0]).not.toHaveProperty("x");
  });

  it("passes caller-owned node and edge metadata through without cloning or interpreting it", () => {
    const nodeMetadata = { owner: "platform", nested: { priority: 1 } };
    const edgeMetadata = { protocol: "HTTPS", flags: ["external"] };
    const shaped = shapeGraphData({
      nodes: [
        { id: "gateway", metadata: nodeMetadata },
        { id: "api" },
      ],
      edges: [{ id: "route", source: "gateway", target: "api", metadata: edgeMetadata }],
    });

    expect(shaped.nodes[0]?.metadata).toBe(nodeMetadata);
    expect(shaped.edges[0]?.metadata).toBe(edgeMetadata);
    expect(nodeMetadata).toEqual({ owner: "platform", nested: { priority: 1 } });
    expect(edgeMetadata).toEqual({ protocol: "HTTPS", flags: ["external"] });
  });

  it("rejects duplicate nodes and edges that point outside the graph", () => {
    expect(() =>
      shapeGraphData({ nodes: [{ id: "same" }, { id: "same" }], edges: [] }),
    ).toThrow(/Duplicate graph node/);
    expect(() =>
      shapeGraphData({ nodes: [{ id: "known" }], edges: [{ source: "known", target: "missing" }] }),
    ).toThrow(/unknown node/);
  });
});

describe("graph hub layout", () => {
  it("pins the force tuning that dissuades hubs and contracts dense neighbourhoods", () => {
    expect({
      minimumIterations: GRAPH_LAYOUT_MIN_ITERATIONS,
      maximumIterations: GRAPH_LAYOUT_MAX_ITERATIONS,
      iterationsPerNode: GRAPH_LAYOUT_ITERATIONS_PER_NODE,
      gravity: GRAPH_LAYOUT_GRAVITY,
      scalingRatio: GRAPH_LAYOUT_SCALING_RATIO,
      stagePadding: GRAPH_ENGINE_STAGE_PADDING,
    }).toEqual({
      minimumIterations: 20,
      maximumIterations: 100,
      iterationsPerNode: 2,
      gravity: 0.08,
      scalingRatio: 8,
      stagePadding: 24,
    });
  });

  it("collapses selected communities into sized supernodes and expands back to the original grouping", () => {
    const collapsed = collapseGraphCommunities(islandData, ["alpha"]);
    const supernode = collapsed.nodes.find((node) => node.collapsedCommunity === "alpha");

    expect(supernode).toMatchObject({
      label: "alpha (2)",
      community: "alpha",
      memberIds: ["a1", "a2"],
    });
    expect(supernode?.size).toBeGreaterThan(islandData.nodes[0]!.size);
    expect(collapsed.nodes.map((node) => node.id)).not.toContain("a1");
    expect(collapsed.edges.find((edge) => edge.id === "cross-1")?.source).toBe(supernode?.id);
    expect(collapsed.edges.some((edge) => edge.id === "inside-alpha")).toBe(false);
    expect(collapseGraphCommunities(islandData, [])).toEqual(islandData);
  });

  it("separates top backbone-betweenness trunks and keeps each subtree closest to its trunk", () => {
    const layout = layoutGraphByCentrality(multiHubData, { seed: 397, hubCount: 3 });
    const nodes = new Map(layout.data.nodes.map((node) => [node.id, node]));
    const distance = (left: string, right: string): number => {
      const a = nodes.get(left)!;
      const b = nodes.get(right)!;
      return Math.hypot(a.x - b.x, a.y - b.y);
    };

    expect(layout.hubs).toEqual(["hub-a", "hub-b", "hub-c"]);
    for (let left = 0; left < layout.hubs.length; left += 1) {
      for (let right = left + 1; right < layout.hubs.length; right += 1) {
        expect(distance(layout.hubs[left]!, layout.hubs[right]!)).toBeGreaterThan(0.9);
      }
    }
    for (const hub of layout.hubs) {
      for (const neighbour of layout.childrenByNode[hub] ?? []) {
        expect(distance(hub, neighbour)).toBeLessThan(
          Math.min(...layout.hubs.filter((other) => other !== hub).map((other) => distance(other, neighbour))),
        );
      }
    }
  });

  it("is deterministic for a fixed seed", () => {
    expect(layoutGraphByCentrality(multiHubData, { seed: 397, hubCount: 3 })).toEqual(
      layoutGraphByCentrality(multiHubData, { seed: 397, hubCount: 3 }),
    );
    expect(layoutGraphByCentrality(multiHubData, { seed: 398, hubCount: 3 }).data.nodes).not.toEqual(
      layoutGraphByCentrality(multiHubData, { seed: 397, hubCount: 3 }).data.nodes,
    );
  });

  it("materially reduces crossings from the previous force-only fixture arrangement", () => {
    const baseline = countGraphEdgeCrossings(multiHubData);
    const laidOut = layoutGraphByCentrality(multiHubData, { seed: 397, hubCount: 3 }).data;

    expect(baseline).toBeGreaterThan(0);
    expect(countGraphEdgeCrossings(laidOut)).toBeLessThanOrEqual(Math.floor(baseline * 0.5));
  });
});

describe("graph communities and search", () => {
  it("groups communities in input order and assigns only DS color tokens", () => {
    const communities = groupGraphCommunities(shapeGraphData(data).nodes);

    expect(communities.map(({ id, nodes }) => ({ id, nodes: nodes.map((node) => node.id) }))).toEqual([
      { id: "services", nodes: ["api"] },
      { id: "storage", nodes: ["db", "cache"] },
    ]);
    expect(communities.map((community) => community.color)).toEqual([
      GRAPH_COLOR_TOKENS[0],
      GRAPH_COLOR_TOKENS[1],
    ]);
    expect(communities.every((community) => community.color.startsWith("var(--reddb-color-"))).toBe(true);
  });

  it("finds nodes by id, label, or community without changing caller order", () => {
    const nodes = shapeGraphData(data).nodes;
    expect(searchGraphNodes(nodes, "stor").map((node) => node.id)).toEqual(["db", "cache"]);
    expect(searchGraphNodes(nodes, "API").map((node) => node.id)).toEqual(["api"]);
    expect(searchGraphNodes(nodes, "")).toEqual(nodes);
  });
});

describe("graph focus", () => {
  it("grows the focused node and its immediate neighbours while shrinking the rest", () => {
    const shaped = shapeGraphData({
      nodes: [{ id: "focus" }, { id: "neighbour" }, { id: "rest" }],
      edges: [{ source: "focus", target: "neighbour" }],
    });

    const focused = reduceGraphFocus(shaped, "focus");

    expect([GRAPH_FOCUSED_SCALE, GRAPH_NEIGHBOUR_SCALE, GRAPH_REST_SCALE]).toEqual([
      1.3, 1.12, 0.9,
    ]);
    expect(focused.nodes.map(({ id, scale }) => ({ id, scale }))).toEqual([
      { id: "focus", scale: 1.3 },
      { id: "neighbour", scale: 1.12 },
      { id: "rest", scale: 0.9 },
    ]);
  });

  it("mutes nodes outside the focused fan", () => {
    const shaped = shapeGraphData({
      nodes: [{ id: "focus" }, { id: "neighbour" }, { id: "rest" }],
      edges: [{ source: "focus", target: "neighbour" }],
    });

    const focused = reduceGraphFocus(shaped, "focus");

    expect(focused.nodes.map(({ id, color }) => ({ id, color }))).toEqual([
      { id: "focus", color: GRAPH_COLOR_TOKENS[0] },
      { id: "neighbour", color: GRAPH_COLOR_TOKENS[0] },
      { id: "rest", color: "var(--reddb-color-ink-muted)" },
    ]);
    expect(GRAPH_MUTED_COLOR_TOKEN).toBe("var(--reddb-color-ink-muted)");
  });

  it("shows labels only for a focused node and its small neighbourhood", () => {
    const shaped = shapeGraphData({
      nodes: [{ id: "focus" }, { id: "neighbour" }, { id: "rest" }],
      edges: [{ source: "focus", target: "neighbour" }],
    });

    expect(
      reduceGraphFocus(shaped, "focus").nodes.map(({ id, showLabel }) => ({ id, showLabel })),
    ).toEqual([
      { id: "focus", showLabel: true },
      { id: "neighbour", showLabel: true },
      { id: "rest", showLabel: false },
    ]);
    expect(reduceGraphFocus(shaped, null).nodes.every((node) => !node.showLabel)).toBe(true);
  });

  it("drops neighbour labels when the focused neighbourhood exceeds the named limit", () => {
    const neighbours = Array.from({ length: 36 }, (_, index) => ({ id: `node-${index}` }));
    const shaped = shapeGraphData({
      nodes: [{ id: "focus" }, ...neighbours],
      edges: neighbours.map((node) => ({ source: "focus", target: node.id })),
    });

    const visible = reduceGraphFocus(shaped, "focus").nodes
      .filter((node) => node.showLabel)
      .map((node) => node.id);

    expect(GRAPH_NEIGHBOUR_LABEL_LIMIT).toBe(36);
    expect(visible).toEqual(["focus"]);
  });

  it("adds the reference pulse to the focused scale without changing its resting emphasis", () => {
    const shaped = shapeGraphData({ nodes: [{ id: "focus" }], edges: [] });

    expect(GRAPH_FOCUS_PULSE_SCALE).toBe(0.05);
    expect(reduceGraphFocus(shaped, "focus", 0).nodes[0]?.scale).toBe(1.3);
    expect(reduceGraphFocus(shaped, "focus", 1).nodes[0]?.scale).toBe(1.35);
  });

  it("keeps edge labels hidden at rest and surfaces truncated names on the focused fan", () => {
    const shaped = shapeGraphData({
      nodes: [{ id: "focus" }, { id: "neighbour" }, { id: "rest" }],
      edges: [
        { source: "focus", target: "neighbour", label: "abcdefghijklmnopqrstuvwxyz123456" },
        { source: "neighbour", target: "rest", label: "background" },
      ],
    });

    expect(reduceGraphFocus(shaped, null).edges.map((edge) => edge.displayLabel)).toEqual([
      undefined,
      undefined,
    ]);
    expect(reduceGraphFocus(shaped, "focus").edges.map((edge) => edge.displayLabel)).toEqual([
      "abcdefghijklmnopqrstuvwxyz1…",
      undefined,
    ]);
    expect(GRAPH_EDGE_LABEL_MAX_LENGTH).toBe(28);
  });

  it("mutes edges outside the focused fan", () => {
    const shaped = shapeGraphData({
      nodes: [{ id: "focus" }, { id: "neighbour" }, { id: "rest" }],
      edges: [
        { source: "focus", target: "neighbour" },
        { source: "neighbour", target: "rest" },
      ],
    });

    expect(reduceGraphFocus(shaped, "focus").edges.map((edge) => edge.color)).toEqual([
      "var(--reddb-color-control-edge)",
      "var(--reddb-color-ink-muted)",
    ]);
  });
});

describe("Graph", () => {
  it.each([
    { name: "showcase", data: showcaseData },
    {
      name: "dense",
      data: {
        nodes: Array.from({ length: 64 }, (_, index) => ({
          id: `dense-${index}`,
          label: `Service ${index}`,
          size: index % 11 === 0 ? 11 : 6,
        })),
        edges: Array.from({ length: 63 }, (_, index) => ({
          source: `dense-${Math.floor(index / 2)}`,
          target: `dense-${index + 1}`,
        })),
      },
    },
  ])("fills both rendered canvas axes for the $name fixture at several aspect ratios", ({ data }) => {
    for (const [width, height] of [[1_012, 480], [640, 640], [480, 760]] as const) {
      const root = render({
        data,
        renderer: "svg",
        width,
        height,
        layoutSeed: 397,
        hubCount: 3,
        searchable: false,
      });
      const circles = renderedCircles(root);
      const left = Math.min(...circles.map((circle) => circle.x - circle.radius));
      const right = Math.max(...circles.map((circle) => circle.x + circle.radius));
      const top = Math.min(...circles.map((circle) => circle.y - circle.radius));
      const bottom = Math.max(...circles.map((circle) => circle.y + circle.radius));

      expect((right - left) / (width - 80), `${width}×${height} horizontal fill`)
        .toBeGreaterThanOrEqual(GRAPH_LAYOUT_MIN_AXIS_FILL);
      expect((bottom - top) / (height - 80), `${width}×${height} vertical fill`)
        .toBeGreaterThanOrEqual(GRAPH_LAYOUT_MIN_AXIS_FILL);
      expect(left).toBeGreaterThanOrEqual(0);
      expect(right).toBeLessThanOrEqual(width);
      expect(top).toBeGreaterThanOrEqual(0);
      expect(bottom).toBeLessThanOrEqual(height);
    }
  });

  it.each(["tiny", "compact", "comfortable", "spacious"] as const)(
    "keeps a radius-proportional edge gap at rest and after focusing a hub at %s Density",
    (density) => {
      document.documentElement.dataset.density = density;
      const root = render({
        data: showcaseData,
        renderer: "svg",
        width: 1_012,
        height: 480,
        layoutSeed: 397,
        hubCount: 3,
        searchable: false,
      });
      const assertGaps = (): void => {
        const circles = renderedCircles(root);
        for (let left = 0; left < circles.length; left += 1) {
          for (let right = left + 1; right < circles.length; right += 1) {
            const first = circles[left]!;
            const second = circles[right]!;
            const requiredGap = Math.min(first.radius, second.radius) * minimumGapRatio(density);
            const edgeGap = Math.hypot(first.x - second.x, first.y - second.y) -
              first.radius - second.radius;
            expect(edgeGap + 1e-6, `${first.id} / ${second.id}`).toBeGreaterThanOrEqual(requiredGap);
          }
        }
      };

      assertGaps();
      root.querySelector<SVGCircleElement>('[data-node-id="api"]')!
        .dispatchEvent(new MouseEvent("click", { bubbles: true }));
      flushSync();
      assertGaps();
      document.documentElement.removeAttribute("data-density");
    },
  );

  it("includes readable labels in the node separation footprint", () => {
    const root = render({
      data: showcaseData,
      renderer: "svg",
      width: 1_012,
      height: 480,
      layoutSeed: 397,
      hubCount: 3,
      searchable: false,
    });
    const labels = new Map(showcaseData.nodes.map((node) => [node.id, node.label]));
    const circles = renderedCircles(root);

    for (let left = 0; left < circles.length; left += 1) {
      for (let right = left + 1; right < circles.length; right += 1) {
        const first = circles[left]!;
        const second = circles[right]!;
        const footprint = (circle: RenderedCircle): number => Math.max(
          circle.radius,
          Math.min(circle.radius * 2, Math.max(16, labels.get(circle.id)!.length * 7.5) / 2),
        );
        expect(Math.hypot(first.x - second.x, first.y - second.y) + 1e-6)
          .toBeGreaterThanOrEqual(footprint(first) + footprint(second) + 4);
      }
    }
  });

  it("renders a crowded hub without overlapping node circles", () => {
    const nodes = Array.from({ length: 15 }, (_, index) => ({
      id: `n${index}`,
      size: index === 0 ? 13 : 8,
    }));
    const root = render({
      data: {
        nodes,
        edges: nodes.slice(1).map((node) => ({ source: "n0", target: node.id })),
      },
      renderer: "svg",
      width: 1_012,
      height: 480,
      layoutSeed: 415,
      hubCount: 1,
      searchable: false,
    });
    const circles = [...root.querySelectorAll<SVGCircleElement>("circle[data-node-id]")].map(
      (circle) => ({
        x: Number(circle.getAttribute("cx")),
        y: Number(circle.getAttribute("cy")),
        radius: Number(circle.getAttribute("r")),
      }),
    );
    let overlaps = 0;
    for (let left = 0; left < circles.length; left += 1) {
      for (let right = left + 1; right < circles.length; right += 1) {
        const first = circles[left]!;
        const second = circles[right]!;
        if (
          Math.hypot(first.x - second.x, first.y - second.y) + 1e-6 <
          first.radius + second.radius + 4
        ) overlaps += 1;
      }
    }

    expect(overlaps).toBe(0);
  });

  it("inherits the spacious Density margin for node separation", () => {
    document.documentElement.dataset.density = "spacious";
    const nodes = Array.from({ length: 15 }, (_, index) => ({ id: `n${index}`, size: 8 }));
    const root = render({
      data: {
        nodes,
        edges: nodes.slice(1).map((node) => ({ source: "n0", target: node.id })),
      },
      renderer: "svg",
      width: 1_012,
      height: 480,
      layoutSeed: 415,
      hubCount: 1,
      searchable: false,
    });
    const circles = [...root.querySelectorAll<SVGCircleElement>("circle[data-node-id]")].map(
      (circle) => ({
        x: Number(circle.getAttribute("cx")),
        y: Number(circle.getAttribute("cy")),
        radius: Number(circle.getAttribute("r")),
      }),
    );
    document.documentElement.removeAttribute("data-density");

    for (let left = 0; left < circles.length; left += 1) {
      for (let right = left + 1; right < circles.length; right += 1) {
        const first = circles[left]!;
        const second = circles[right]!;
        expect(Math.hypot(first.x - second.x, first.y - second.y) + 1e-6).toBeGreaterThanOrEqual(
          first.radius + second.radius + 8,
        );
      }
    }
  });

  it("uses detected communities for the legend and grouping contract", () => {
    const nodes = Array.from({ length: 8 }, (_, index) => ({ id: `member-${index}` }));
    const edges = [0, 4].flatMap((start) =>
      [0, 1, 2, 3].flatMap((left) =>
        [0, 1, 2, 3]
          .filter((right) => right > left)
          .map((right) => ({
            source: `member-${start + left}`,
            target: `member-${start + right}`,
          })),
      ),
    );
    edges.push({ source: "member-3", target: "member-4" });

    const root = render({ data: { nodes, edges }, renderer: "svg", layoutSeed: 406 });
    const legend = [...root.querySelectorAll('ul[aria-label="Graph communities"] li')]
      .map((item) => item.textContent?.trim());

    expect(legend).toHaveLength(2);
    expect(legend.every((label) => !label?.includes("unassigned"))).toBe(true);
  });

  it("caps focused growth by both its resting radius and the rendered neighbourhood spacing", () => {
    const fixtures = [
      {
        name: "dense small canvas",
        width: 180,
        height: 160,
        data: {
          nodes: [
            { id: "hub", size: 13 },
            ...Array.from({ length: 8 }, (_, index) => ({ id: `dense-${index}`, size: 10 })),
          ],
          edges: Array.from({ length: 8 }, (_, index) => ({
            source: "hub",
            target: `dense-${index}`,
          })),
        },
      },
      {
        name: "sparse canvas",
        width: 760,
        height: 480,
        data: {
          nodes: [
            { id: "hub", size: 13 },
            { id: "leaf", size: 10 },
          ],
          edges: [{ source: "hub", target: "leaf" }],
        },
      },
    ] as const;

    for (const fixture of fixtures) {
      const root = render({
        data: fixture.data,
        renderer: "svg",
        width: fixture.width,
        height: fixture.height,
        layout: "radial",
        hubCount: 1,
      });
      const hub = root.querySelector<SVGCircleElement>('[data-node-id="hub"]')!;
      const restingRadius = Number(hub.getAttribute("r"));

      hub.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      flushSync();

      const focusedRadius = Number(hub.getAttribute("r"));
      const hubX = Number(hub.getAttribute("cx"));
      const hubY = Number(hub.getAttribute("cy"));
      const nearestNeighbourSpacing = Math.min(
        ...[...root.querySelectorAll<SVGCircleElement>('[data-node-id]:not([data-node-id="hub"])')]
          .map((node) =>
            Math.hypot(
              Number(node.getAttribute("cx")) - hubX,
              Number(node.getAttribute("cy")) - hubY,
            ),
          ),
      );

      expect(focusedRadius, `${fixture.name} resting-radius cap`).toBeLessThanOrEqual(
        restingRadius * 1.35,
      );
      expect(focusedRadius * 2, `${fixture.name} neighbourhood-spacing cap`).toBeLessThanOrEqual(
        nearestNeighbourSpacing,
      );
    }
  });

  it("emits unmodified node and edge metadata for click and focus interactions", () => {
    const nodeMetadata = { owner: "platform", rank: 3 };
    const edgeMetadata = { protocol: "HTTPS", rate: 1200 };
    const oninteraction = vi.fn();
    const root = render({
      data: {
        nodes: [
          { id: "gateway", metadata: nodeMetadata },
          { id: "api" },
        ],
        edges: [
          { id: "route", source: "gateway", target: "api", metadata: edgeMetadata },
        ],
      },
      renderer: "svg",
      oninteraction,
    });

    root
      .querySelector<SVGCircleElement>('[data-node-id="gateway"]')!
      .dispatchEvent(new MouseEvent("click", { bubbles: true }));
    root
      .querySelector<SVGGElement>('[data-edge-id="route"]')!
      .dispatchEvent(new FocusEvent("focus"));

    expect(oninteraction).toHaveBeenCalledWith({
      kind: "node",
      action: "click",
      id: "gateway",
      metadata: nodeMetadata,
    });
    expect(oninteraction).toHaveBeenCalledWith({
      kind: "edge",
      action: "focus",
      id: "route",
      metadata: edgeMetadata,
    });
    expect(oninteraction.mock.calls[0]![0].metadata).toBe(nodeMetadata);
    expect(oninteraction.mock.calls.at(-1)![0].metadata).toBe(edgeMetadata);
  });

  it("expands a collapsed supernode without drawing community containers or bundled edges", () => {
    const oncollapsedcommunitieschange = vi.fn();
    const root = render({
      data: islandData,
      renderer: "svg",
      collapsedCommunities: ["alpha"],
      layout: "radial",
      layoutSeed: 397,
      oncollapsedcommunitieschange,
    });

    expect(root.querySelector('[data-supernode-community="alpha"]')).not.toBeNull();
    expect(root.querySelector("[data-community-box]")).toBeNull();
    expect(root.querySelector("[data-graph-overlay-edge]")).toBeNull();

    root
      .querySelector<SVGCircleElement>('[data-supernode-community="alpha"]')!
      .dispatchEvent(new MouseEvent("click", { bubbles: true }));
    flushSync();

    expect(root.querySelector('[data-supernode-community="alpha"]')).toBeNull();
    expect(root.querySelector('[data-node-id="a1"]')).not.toBeNull();
    expect(oncollapsedcommunitieschange).toHaveBeenCalledWith([]);
  });

  it("automatically collapses and expands deep subtrees above the canvas-derived limit", () => {
    const nodeCount = 900;
    const largeTree = {
      nodes: Array.from({ length: nodeCount }, (_, index) => ({ id: `detail-${index}` })),
      edges: Array.from({ length: nodeCount - 1 }, (_, index) => ({
        source: `detail-${Math.floor(index / 2)}`,
        target: `detail-${index + 1}`,
      })),
    };
    const root = render({
      data: largeTree,
      renderer: "svg",
      width: 360,
      height: 280,
      hubCount: 1,
      layoutSeed: 407,
    });
    const graph = root.querySelector<HTMLElement>("[data-ds-graph]")!;
    const summary = root.querySelector<SVGCircleElement>("[data-summary-subtree]")!;
    const memberCount = Number(summary.dataset.summaryMemberCount);
    const initialVisibleCount = Number(graph.dataset.visibleNodeCount);

    expect(Number(graph.dataset.visibleNodeCount)).toBeLessThanOrEqual(
      Number(graph.dataset.nodeLimit),
    );
    expect(memberCount).toBeGreaterThan(1);

    summary.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    flushSync();

    expect(root.querySelector(`[data-summary-subtree="${summary.dataset.summarySubtree}"]`))
      .toBeNull();
    expect(Number(graph.dataset.visibleNodeCount)).toBeGreaterThan(initialVisibleCount);
  });

  it("renders named edges only after their fan receives focus", () => {
    const namedData = {
      ...data,
      edges: [
        { source: "api", target: "db", label: "Reads from primary datastore" },
        { source: "api", target: "cache", label: "Checks response cache" },
      ],
    };
    const root = render({ data: namedData, renderer: "svg" });

    expect(root.querySelectorAll("[data-graph-edge-label]")).toHaveLength(0);
    root
      .querySelector<SVGCircleElement>('[data-node-id="api"]')!
      .dispatchEvent(new MouseEvent("click", { bubbles: true }));
    flushSync();

    expect(
      [...root.querySelectorAll("[data-graph-edge-label]")].map((label) => label.textContent),
    ).toEqual(["Reads from primary datastore", "Checks response cache"]);
  });

  it("clears node focus when the SVG background is clicked", () => {
    const onfocusednodechange = vi.fn();
    const root = render({ data, renderer: "svg", onfocusednodechange });
    const svg = root.querySelector<SVGSVGElement>("svg")!;
    const node = root.querySelector<SVGCircleElement>("circle")!;

    node.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    flushSync();
    expect(root.querySelector('[data-graph-node][data-focused="true"]')).not.toBeNull();

    svg.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    flushSync();
    expect(root.querySelector('[data-graph-node][data-focused="true"]')).toBeNull();
    expect(onfocusednodechange).toHaveBeenLastCalledWith(null);
  });

  it("holds the focus pulse at rest under reduced motion without removing trunk size emphasis", () => {
    const matchMedia = vi.fn((query: string) => ({
      matches: true,
      media: query,
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    }));
    vi.stubGlobal("matchMedia", matchMedia);

    const root = render({ data, renderer: "svg", focusedNode: "api" });
    const focusedNode = root.querySelector<SVGCircleElement>('[data-node-id="api"]')!;

    expect(matchMedia).toHaveBeenCalledWith("(prefers-reduced-motion: reduce)");
    expect(focusedNode.dataset.focusPulse).toBe("held");
    expect(focusedNode.getAttribute("r")).toBe("12.48");
  });

  it("renders its SVG fallback cleanly across Theme × Color Scheme × Density", () => {
    for (const theme of ["base", "editorial"]) {
      for (const scheme of ["light", "dark"]) {
        for (const density of ["tiny", "compact", "comfortable", "spacious"]) {
          const root = render({ data, label: "Service dependencies", renderer: "svg" });
          const graph = root.querySelector<HTMLElement>("[data-ds-graph]")!;
          graph.dataset.theme = theme;
          graph.dataset.colorScheme = scheme;
          graph.dataset.density = density;

          expect(graph.getAttribute("aria-label")).toBe("Service dependencies");
          const fallback = graph.querySelector('[data-graph-fallback="svg"]');
          expect(fallback).not.toBeNull();
          // A named group, not an img: its focusable nodes and edges must not
          // nest inside a presentational image (wave 5A, nested-interactive).
          expect(fallback!.getAttribute("role")).toBe("group");
          expect(fallback!.querySelector('[role="button"]')).not.toBeNull();
          expect(graph.querySelectorAll("circle")).toHaveLength(3);
          expect(graph.querySelectorAll("line")).toHaveLength(2);
          expect(graph.innerHTML).toContain("var(--reddb-color-");
          expect(graph.textContent).not.toMatch(/undefined|NaN|\[object Object\]/);
        }
      }
    }
    expect(engine.loaded).toBe(false);
  });

  it("keeps sigma and graphology unloaded until the first engine render", async () => {
    expect(engine.loaded).toBe(false);
    const root = render({ data, label: "Service dependencies" });
    expect(root.querySelector('[data-engine-state="loading"]')).not.toBeNull();

    await vi.waitFor(() => expect(engine.mounted).toBe(1));

    expect(engine.loaded).toBe(true);
    expect(root.querySelector('[data-engine-state="ready"]')).not.toBeNull();
  });

  it("draws its community legend in the colour each community's nodes wear, and nothing else", () => {
    const root = render({
      label: "Referral network",
      renderer: "svg",
      searchable: false,
      data: {
        nodes: [
          { id: "a", community: "enterprise", color: GRAPH_COLOR_TOKENS[1] },
          { id: "b", community: "enterprise", color: GRAPH_COLOR_TOKENS[1] },
          { id: "c", community: "starter", color: GRAPH_COLOR_TOKENS[5] },
        ],
        edges: [
          { source: "a", target: "b" },
          { source: "b", target: "c" },
        ],
      },
    });
    const legend = [...root.querySelectorAll<HTMLElement>('[aria-label="Graph communities"] li')];

    expect(legend.map((item) => [item.textContent?.trim(), item.querySelector("span")?.style.background])).toEqual([
      ["enterprise (2)", GRAPH_COLOR_TOKENS[1]],
      ["starter (1)", GRAPH_COLOR_TOKENS[5]],
    ]);
    // No stray source text trails the component's markup.
    expect(root.textContent).not.toMatch(/detectGraphCommunities|filterGraphBackbone/);
  });
});
