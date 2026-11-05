<!-- A progressively enhanced network: accessible SVG first, Sigma only after client render. -->
<script module lang="ts">
  export {
    GRAPH_COLOR_TOKENS,
    GRAPH_EDGE_LABEL_MAX_LENGTH,
    GRAPH_EDGE_COLOR_TOKEN,
    GRAPH_FOCUSED_SCALE,
    GRAPH_FOCUS_PULSE_SCALE,
    GRAPH_ENGINE_STAGE_PADDING,
    GRAPH_LAYOUT_GRAVITY,
    GRAPH_LAYOUT_ITERATIONS_PER_NODE,
    GRAPH_LAYOUT_MAX_ITERATIONS,
    GRAPH_LAYOUT_MIN_AXIS_FILL,
    GRAPH_LAYOUT_MIN_ITERATIONS,
    GRAPH_LAYOUT_SCALING_RATIO,
    GRAPH_MUTED_COLOR_TOKEN,
    GRAPH_NEIGHBOUR_LABEL_LIMIT,
    GRAPH_NEIGHBOUR_SCALE,
    GRAPH_REST_SCALE,
    GRAPH_SUPERNODE_BASE_SIZE,
    GRAPH_SUPERNODE_MEMBER_SCALE,
    analyseGraphStructure,
    applyGraphLevelOfDetail,
    collapseGraphCommunities,
    countGraphEdgeCrossings,
    countGraphNodeOverlapRatio,
    deriveGraphLevelOfDetail,
    detectGraphCommunities,
    filterGraphBackbone,
    graphNormalizedStress,
    groupGraphCommunities,
    layoutGraphByCentrality,
    layoutGraphBySparseStress,
    partitionGraphByCentrality,
    rankGraphByBetweenness,
    reduceGraphFocus,
    searchGraphNodes,
    shapeGraphData,
  } from "./graph";
  export type {
    GraphColorToken,
    GraphBackboneOptions,
    GraphBetweennessOptions,
    GraphBetweennessRank,
    GraphCentralityLayout,
    GraphCentralityLayoutOptions,
    GraphCommunity,
    GraphCommunityDetectionOptions,
    GraphData,
    GraphEdge,
    GraphInteraction,
    GraphLevelOfDetail,
    GraphLevelOfDetailOptions,
    GraphLevelOfDetailProfile,
    GraphMetadata,
    GraphPoint,
    GraphCentralityPartition,
    GraphCentralityPartitionWork,
    GraphSparseStressLayout,
    GraphSparseStressLayoutOptions,
    GraphStructureAnalysis,
    GraphStructureAnalysisOptions,
    FocusedGraphData,
    FocusedGraphEdge,
    FocusedGraphNode,
    GraphFocusViewport,
    GraphNode,
    ShapedGraphData,
    ShapedGraphEdge,
    ShapedGraphNode,
  } from "./graph";
  export type GraphLayout = "stress" | "force" | "radial";
</script>

<script lang="ts">
 import type {GraphContract} from "./core/contracts/graph";
  import { Input, ToggleButton } from "@reddb-io/design-system/base";
  import { onMount, tick, untrack } from "svelte";
  import { warnDeprecated } from "@reddb-io/design-system/base";
  import {
    applyGraphLevelOfDetail,
    collapseGraphCommunities,
    detectGraphCommunities,
    groupGraphCommunities,
    layoutGraphBySparseStress,
    reduceGraphFocus,
    searchGraphNodes,
    shapeGraphData,
    type GraphData,
    type GraphInteraction,
    type ShapedGraphNode,
  } from "./graph";

  type GraphLayout = "stress" | "force" | "radial";

  // Graph nodes and edges are focusable SVG; each draws the same ink focus
  // outline every DS control does (ADR 0023), never the browser's default.
  const GRAPH_FOCUS_OUTLINE =
    "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus";

  interface GraphEngineController {
    destroy(): void;
    focusNode(id: string | null): void;
    setData(
      data: ReturnType<typeof shapeGraphData>,
      options?: { layout: GraphLayout },
    ): void;
  }

  interface Props extends GraphContract {
    /** Reports the focused node id after focus moves to a node, or clears. */
    onfocusednodechange?: (id: string | null) => void;
    /** @deprecated Renamed `onfocusednodechange` (ADR 0026); removed next release. */
    onFocusedNode?: (id: string | null) => void;
    /** Reports the still-collapsed communities after one expands. */
    oncollapsedcommunitieschange?: (communities: readonly string[]) => void;
    /** @deprecated Renamed `oncollapsedcommunitieschange` (ADR 0026); removed next release. */
    onCollapsedCommunitiesChange?: (communities: readonly string[]) => void;
    /** A discrete event: every click and keyboard focus on a node or edge, caller metadata intact. */
    oninteraction?: (interaction: GraphInteraction) => void;
    /** @deprecated Renamed `oninteraction` (ADR 0026); removed next release. */
    onInteraction?: (interaction: GraphInteraction) => void;
    /** Extra classes, merged onto the root element that holds the search field and the drawing. */
    class?: string;
  }

  let {
    data,
    label = "Network graph",
    width = 640,
    height = 400,
    renderer = "auto",
    searchable = true,
    focusedNode,
    onfocusednodechange,
    onFocusedNode,
    collapsedCommunities = [],
    oncollapsedcommunitieschange,
    onCollapsedCommunitiesChange,
    layout = "stress",
    layoutSeed = 0,
    hubCount,
    oninteraction,
    onInteraction,
    showDetails = false,
    class: className = "",
  }: Props = $props();

  let query = $state("");
  let localFocus = $state<string | null>(null);
  let engineState = $state<"fallback" | "loading" | "ready">("loading");
  let engineHost: HTMLDivElement;
  let graphRoot: HTMLElement;
  let controller = $state<GraphEngineController>();
  let reducedMotion = $state(false);
  let nodeMargin = $state(4);
  let localCollapsedCommunities = $state(untrack(() => new Set(collapsedCommunities)));
  let localExpandedSubtrees = $state(new Set<string>());
  let selectedInteraction = $state<GraphInteraction>();

  const shaped = $derived(shapeGraphData(data));
  const detected = $derived(detectGraphCommunities(shaped, { seed: layoutSeed }));
  const communities = $derived(groupGraphCommunities(detected.nodes));
  const collapsed = $derived(collapseGraphCommunities(detected, localCollapsedCommunities));
  const arrangement = $derived(
    layoutGraphBySparseStress(collapsed, {
      seed: layoutSeed,
      hubCount,
      viewport: { width, height, padding: 40, nodeMargin },
      nodeMargin,
    }),
  );
  const detail = $derived(
    applyGraphLevelOfDetail(arrangement.data, arrangement.parentByNode, {
      width,
      height,
      padding: 40,
      expandedSubtrees: localExpandedSubtrees,
    }),
  );
  const visible = $derived(detail.data);
  const results = $derived(searchGraphNodes(visible.nodes, query));
  const activeFocus = $derived(focusedNode === undefined ? localFocus : focusedNode);
  const focused = $derived(
    reduceGraphFocus(visible, activeFocus ?? null, 0, {
      width,
      height,
      padding: 40,
      nodeMargin,
    }),
  );

  function nodeById(id: string): ShapedGraphNode {
    return visible.nodes.find((node) => node.id === id)!;
  }

  function svgX(node: ShapedGraphNode): number {
    return ((node.x + 1) / 2) * (width - 80) + 40;
  }

  function svgY(node: ShapedGraphNode): number {
    return ((node.y + 1) / 2) * (height - 80) + 40;
  }

  interface SvgLabelBox {
    left: number;
    right: number;
    top: number;
    bottom: number;
  }

  interface SvgLabel {
    key: string;
    text: string;
    x: number;
    y: number;
    box: SvgLabelBox;
    nodeId?: string;
    edgeId?: string;
  }

  function labelBox(text: string, x: number, y: number): SvgLabelBox {
    const labelWidth = Math.max(16, text.length * 7.5);
    return {
      left: x - labelWidth / 2,
      right: x + labelWidth / 2,
      top: y - 12,
      bottom: y + 4,
    };
  }

  function boxesOverlap(left: SvgLabelBox, right: SvgLabelBox): boolean {
    return (
      left.left < right.right + 2 &&
      left.right > right.left - 2 &&
      left.top < right.bottom + 2 &&
      left.bottom > right.top - 2
    );
  }

  const svgLabels = $derived.by((): SvgLabel[] => {
    const circles = focused.nodes.map((node) => ({
      x: svgX(node),
      y: svgY(node),
      radius: node.size * node.scale + 3,
    }));
    const occupied: SvgLabelBox[] = [];
    const labels: SvgLabel[] = [];
    const fits = (box: SvgLabelBox): boolean => {
      if (box.left < 4 || box.right > width - 4 || box.top < 4 || box.bottom > height - 4) {
        return false;
      }
      if (occupied.some((other) => boxesOverlap(box, other))) return false;
      return circles.every((circle) => {
        const nearestX = Math.max(box.left, Math.min(circle.x, box.right));
        const nearestY = Math.max(box.top, Math.min(circle.y, box.bottom));
        return Math.hypot(circle.x - nearestX, circle.y - nearestY) > circle.radius + 2;
      });
    };
    const accept = (
      key: string,
      text: string,
      candidates: readonly { x: number; y: number }[],
      identity: Pick<SvgLabel, "nodeId" | "edgeId">,
      required = false,
    ): void => {
      let placement = candidates
        .map(({ x, y }) => ({ x, y, box: labelBox(text, x, y) }))
        .find(({ box }) => fits(box));
      if (!placement && required) {
        const labelWidth = Math.max(16, text.length * 7.5);
        for (let y = 18; y <= height - 8 && !placement; y += 18) {
          for (let x = labelWidth / 2 + 6; x <= width - labelWidth / 2 - 6; x += 18) {
            const box = labelBox(text, x, y);
            if (fits(box)) placement = { x, y, box };
          }
        }
      }
      if (!placement) return;
      occupied.push(placement.box);
      labels.push({ key, text, ...placement, ...identity });
    };

    const degrees = new Map(visible.nodes.map((node) => [node.id, 0]));
    for (const edge of visible.edges) {
      degrees.set(edge.source, (degrees.get(edge.source) ?? 0) + 1);
      degrees.set(edge.target, (degrees.get(edge.target) ?? 0) + 1);
    }
    const nodePriority = focused.nodes
      .filter((node) => node.showLabel)
      .map((node, index) => ({ node, index }))
      .sort((left, right) => {
        if (left.node.id === activeFocus) return -1;
        if (right.node.id === activeFocus) return 1;
        const byTrunkRank =
          (left.node.trunkRank ?? Number.POSITIVE_INFINITY) -
          (right.node.trunkRank ?? Number.POSITIVE_INFINITY);
        if (byTrunkRank !== 0) return byTrunkRank;
        return (
          (degrees.get(right.node.id) ?? 0) - (degrees.get(left.node.id) ?? 0) ||
          left.index - right.index
        );
      });

    for (const { node } of nodePriority) {
      const x = svgX(node);
      const y = svgY(node);
      const radius = node.size * node.scale + 3;
      const labelWidth = Math.max(16, node.label.length * 7.5);
      const candidates = [
        { x, y: y + radius + 16 },
        { x, y: y - radius - 8 },
        { x: x + radius + 8 + labelWidth / 2, y: y + 4 },
        { x: x - radius - 8 - labelWidth / 2, y: y + 4 },
        { x: x + radius + 8 + labelWidth / 2, y: y + radius + 12 },
        { x: x - radius - 8 - labelWidth / 2, y: y + radius + 12 },
        { x: x + radius + 8 + labelWidth / 2, y: y - radius - 4 },
        { x: x - radius - 8 - labelWidth / 2, y: y - radius - 4 },
      ];
      accept(
        `node:${node.id}`,
        node.label,
        candidates,
        { nodeId: node.id },
        node.id === activeFocus,
      );
    }

    for (const edge of focused.edges) {
      if (!edge.displayLabel) continue;
      const source = nodeById(edge.source);
      const target = nodeById(edge.target);
      const sourceX = svgX(source);
      const sourceY = svgY(source);
      const targetX = svgX(target);
      const targetY = svgY(target);
      const length = Math.max(1, Math.hypot(targetX - sourceX, targetY - sourceY));
      const normalX = -(targetY - sourceY) / length;
      const normalY = (targetX - sourceX) / length;
      const candidates = [0.5, 0.35, 0.65, 0.2, 0.8].flatMap((progress) =>
        [0, 18, -18, 32, -32].map((offset) => ({
          x: sourceX + (targetX - sourceX) * progress + normalX * offset,
          y: sourceY + (targetY - sourceY) * progress + normalY * offset + 4,
        })),
      );
      accept(`edge:${edge.id}`, edge.displayLabel, candidates, { edgeId: edge.id });
    }

    return labels;
  });

  function interaction(kind: "node" | "edge", action: "click" | "focus", id: string): void {
    const item = kind === "node"
      ? visible.nodes.find((node) => node.id === id)
      : visible.edges.find((edge) => edge.id === id);
    if (!item) return;
    const event: GraphInteraction = {
      kind,
      action,
      id,
      ...(item.metadata === undefined ? {} : { metadata: item.metadata }),
    };
    selectedInteraction = event;
    oninteraction?.(event);
    onInteraction?.(event);
  }

  // Each deprecated camelCase name still fires beside its canonical callback
  // for one release and warns once in development (ADR 0026).
  $effect(() => {
    if (onFocusedNode) warnDeprecated("Graph", "onFocusedNode", "onfocusednodechange");
    if (onCollapsedCommunitiesChange) {
      warnDeprecated("Graph", "onCollapsedCommunitiesChange", "oncollapsedcommunitieschange");
    }
    if (onInteraction) warnDeprecated("Graph", "onInteraction", "oninteraction");
  });

  function expandCommunity(community: string): void {
    const next = new Set(localCollapsedCommunities);
    next.delete(community);
    localCollapsedCommunities = next;
    oncollapsedcommunitieschange?.([...next]);
    onCollapsedCommunitiesChange?.([...next]);
  }

  function expandSubtree(subtree: string): void {
    const next = new Set(localExpandedSubtrees);
    next.add(subtree);
    localExpandedSubtrees = next;
  }

  function focus(id: string | null): void {
    localFocus = id;
    controller?.focusNode(id);
    onfocusednodechange?.(id);
    onFocusedNode?.(id);
    if (id !== null && !reducedMotion) void pulseSvgNode(id);
  }

  async function pulseSvgNode(id: string): Promise<void> {
    await tick();
    const node = [...graphRoot.querySelectorAll<SVGCircleElement>("[data-node-id]")].find(
      (circle) => circle.dataset.nodeId === id,
    );
    node?.animate?.(
      [
        { strokeWidth: "3" },
        { strokeWidth: "5" },
        { strokeWidth: "3" },
      ],
      { duration: 420, easing: "ease-out" },
    );
  }

  function handleNodeClick(event: MouseEvent, id: string): void {
    event.stopPropagation();
    interaction("node", "click", id);
    const node = nodeById(id);
    if (node.collapsedSubtree) {
      expandSubtree(node.collapsedSubtree);
      return;
    }
    if (node.collapsedCommunity) {
      expandCommunity(node.collapsedCommunity);
      return;
    }
    focus(id);
  }

  function handleNodeFocus(id: string): void {
    interaction("node", "focus", id);
  }

  function handleNodeKeydown(event: KeyboardEvent, id: string): void {
    if (event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    handleNodeClick(event as unknown as MouseEvent, id);
  }

  function handleEdgeClick(event: MouseEvent, id: string): void {
    event.stopPropagation();
    interaction("edge", "click", id);
  }

  function handleEdgeKeydown(event: KeyboardEvent, id: string): void {
    if (event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    interaction("edge", "click", id);
  }

  $effect(() => {
    controller?.setData(visible, { layout });
  });

  // The spacing nodes keep apart is the gap-sm role at the Density in effect
  // here — read off a probe sized by the role, never by naming a stop, so a
  // Density island and every stop (tiny included) resolve on their own.
  let densityProbe: HTMLElement;
  onMount(() => {
    const updateNodeMargin = (): void => {
      const width = densityProbe.getBoundingClientRect().width;
      if (width > 0) nodeMargin = width;
    };
    updateNodeMargin();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(updateNodeMargin);
    observer.observe(densityProbe);
    return () => observer.disconnect();
  });

  onMount(() => {
    if (typeof globalThis.matchMedia !== "function") return;
    const preference = globalThis.matchMedia("(prefers-reduced-motion: reduce)");
    const update = (): void => {
      reducedMotion = preference.matches;
    };
    update();
    preference.addEventListener?.("change", update);
    return () => preference.removeEventListener?.("change", update);
  });

  $effect(() => {
    controller?.focusNode(activeFocus ?? null);
  });

  onMount(() => {
    if (renderer === "svg") {
      engineState = "fallback";
      return;
    }
    let active = true;
    void import("./graph-engine")
      .then(({ mountGraphEngine }) => {
        if (!active) return;
        controller = mountGraphEngine(
          engineHost,
          visible,
          (kind: "node" | "edge", id: string) => {
            interaction(kind, "click", id);
            if (kind !== "node") return;
            const node = nodeById(id);
            if (node.collapsedSubtree) expandSubtree(node.collapsedSubtree);
            else if (node.collapsedCommunity) expandCommunity(node.collapsedCommunity);
            else focus(id);
          },
          { layout },
        );
        controller.focusNode(activeFocus ?? null);
        engineState = "ready";
      })
      .catch(() => {
        if (active) engineState = "fallback";
      });
    return () => {
      active = false;
      controller?.destroy();
      controller = undefined;
    };
  });
</script>

<section
  bind:this={graphRoot}
  data-ds-graph
  data-graph-layout={layout}
  data-node-limit={detail.nodeLimit}
  data-visible-node-count={visible.nodes.length}
  data-engine-state={engineState}
  data-node-margin={nodeMargin}
  aria-label={label}
  class={`relative w-full min-w-0 overflow-hidden rounded-lg border border-muted bg-transparent text-foreground ${className}`.trim()}
>
  <span
    bind:this={densityProbe}
    data-graph-density-probe
    aria-hidden="true"
    class="pointer-events-none invisible absolute start-0 top-0 h-0 w-[var(--reddb-spatial-gap-sm)]"
  ></span>
  {#if searchable}
    <div class="border-b border-muted p-[var(--reddb-spatial-inset-sm)]">
      <Input
        bind:value={query}
        type="search"
        aria-label={`Search ${label}`}
        placeholder="Search nodes"
        class="w-full"
      />
      {#if query}
        <div data-graph-search-results class="mt-[var(--reddb-spatial-gap-sm)] flex flex-wrap gap-[var(--reddb-spatial-gap-sm)]">
          {#each results as node (node.id)}
            <ToggleButton
              label={node.label}
              pressed={activeFocus === node.id}
              allowUnpress={false}
              onclick={() => focus(node.id)}
            />
          {:else}
            <span class="text-sm text-ink-muted">No matching nodes.</span>
          {/each}
        </div>
      {/if}
    </div>
  {/if}

  <div class="relative w-full" style:height={`${height}px`}>
    <div
      bind:this={engineHost}
      data-graph-engine
      aria-hidden={engineState !== "ready"}
      class="absolute inset-0"
      class:invisible={engineState !== "ready"}
    ></div>

    <!--
      A group, not an img: an img's children are presentational, so the
      focusable node and edge buttons inside it were nested-interactive and
      unreachable by assistive technology (wave 5A). A named group keeps
      them in the tree as the interactive parts of one graph.
    -->
    <svg
      data-graph-fallback="svg"
      role="group"
      aria-label={label}
      aria-hidden={engineState === "ready"}
      viewBox={`0 0 ${width} ${height}`}
      class="h-full w-full bg-[var(--reddb-color-background)]"
      class:invisible={engineState === "ready"}
      onclick={() => focus(null)}
    >
      <title>{label}</title>
      {#each focused.edges as edge (edge.id)}
        {@const source = nodeById(edge.source)}
        {@const target = nodeById(edge.target)}
        <g
          data-graph-edge
          data-edge-id={edge.id}
          role="button"
          tabindex="0"
          aria-label={edge.label ?? `${source.label} to ${target.label}`}
          class={GRAPH_FOCUS_OUTLINE}
          onclick={(event) => handleEdgeClick(event, edge.id)}
          onfocus={() => interaction("edge", "focus", edge.id)}
          onkeydown={(event) => handleEdgeKeydown(event, edge.id)}
        >
          <line
            x1={svgX(source)}
            y1={svgY(source)}
            x2={svgX(target)}
            y2={svgY(target)}
            stroke={edge.color}
            stroke-width={Math.max(1, edge.weight)}
          />
        </g>
      {/each}
      {#each focused.nodes as node (node.id)}
        <g data-graph-node data-focused={activeFocus === node.id}>
          <circle
            data-node-id={node.id}
            data-supernode-community={node.collapsedCommunity}
            data-summary-subtree={node.collapsedSubtree}
            data-summary-member-count={node.collapsedSubtree ? node.memberIds?.length : undefined}
            data-focus-pulse={activeFocus === node.id ? (reducedMotion ? "held" : "active") : "none"}
            cx={svgX(node)}
            cy={svgY(node)}
            r={node.size * node.scale}
            fill={node.color}
            stroke={activeFocus === node.id ? "var(--reddb-color-foreground)" : "var(--reddb-color-background)"}
            stroke-width={activeFocus === node.id ? 3 : 1}
            role="button"
            tabindex="0"
            aria-label={`${node.label}, community ${node.community}`}
            class={GRAPH_FOCUS_OUTLINE}
            onclick={(event) => handleNodeClick(event, node.id)}
            onfocus={() => handleNodeFocus(node.id)}
            onkeydown={(event) => handleNodeKeydown(event, node.id)}
          />
        </g>
      {/each}
      <g data-graph-label-layer aria-hidden="true">
        {#each svgLabels as label (label.key)}
          <text
            data-graph-node-label={label.nodeId}
            data-graph-edge-label={label.edgeId}
            x={label.x}
            y={label.y}
            text-anchor="middle"
            fill="var(--reddb-color-foreground)"
            class="text-xs"
            style:pointer-events="bounding-box"
          >{label.text}</text>
        {/each}
      </g>
    </svg>
  </div>

  <ul aria-label="Graph communities" class="m-0 flex list-none flex-wrap gap-[var(--reddb-spatial-gap-md)] border-t border-muted p-[var(--reddb-spatial-inset-sm)] text-sm">
    {#each communities as community (community.id)}
      <li class="flex items-center gap-[var(--reddb-spatial-gap-sm)]">
        <span aria-hidden="true" class="inline-block size-2 rounded-full" style:background={community.color}></span>
        {community.label} ({community.nodes.length})
      </li>
    {/each}
  </ul>

  {#if showDetails && selectedInteraction}
    <aside
      data-graph-details
      aria-live="polite"
      class="border-t border-muted p-[var(--reddb-spatial-inset-sm)] text-sm"
    >
      <strong>{selectedInteraction.kind === "node" ? "Node" : "Edge"} {selectedInteraction.id}</strong>
      {#if selectedInteraction.metadata}
        <pre class="mt-[var(--reddb-spatial-gap-sm)] overflow-auto whitespace-pre-wrap text-xs">{JSON.stringify(selectedInteraction.metadata, null, 2)}</pre>
      {:else}
        <span class="ml-[var(--reddb-spatial-gap-sm)] text-ink-muted">No metadata</span>
      {/if}
    </aside>
  {/if}
</section>
