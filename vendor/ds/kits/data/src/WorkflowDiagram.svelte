<script module lang="ts">
  export type {
    WorkflowData,
    WorkflowNode,
    WorkflowPort,
    WorkflowEdge,
  } from "./workflow";
</script>

<script lang="ts">
  import { operationalView } from "./operational.variants";

  import { observeActivity } from "@reddb-io/design-system/base";
  import SceneViewport from "./SceneViewport.svelte";
  import Mark from "./internal/VisualizationMark.svelte";
  import { operationalToneInk } from "./operational";
  import { workflowLayout, workflowFocus, workflowNodeLabel } from "./workflow";
  import {
    ink,
    mutedInk,
    surface,
    edge,
    seriesColor,
    short,
  } from "./visualization";
  import type { WorkflowDiagramContract } from "./core/contracts/workflowdiagram";
  interface Props extends WorkflowDiagramContract {
    /** Additional root classes. */
    class?: string;
  }
  let {
    label,
    data,
    direction = "right",
    selected = null,
    focused = null,
    interactive = false,
    playing = false,
    size = "md",
    onselectedchange,
    class: className,
  }: Props = $props();
  let host = $state<HTMLDivElement>(),
    allowed = $state(false),
    unit = $state(0);
  const scene = $derived(workflowLayout(data, direction, unit || 32));
  const focus = $derived(workflowFocus(scene, focused));
  $effect(() => {
    if (host) return observeActivity(host, (next) => (allowed = next));
  });
  const visual = operationalView();
</script>

<div
  bind:this={host}
  data-workflow-diagram
  class={visual.scene({ class: className })}
>
  <span aria-hidden="true" bind:clientHeight={unit} class={visual.probe()}
  ></span>
  <p class={visual.caption()}>{label}</p>
  <SceneViewport
    {label}
    contentWidth={scene.width}
    contentHeight={scene.height}
    {focus}
    {size}
    ><svg
      width={scene.width}
      height={scene.height}
      role="group"
      aria-label={label}
      ><title>{label}</title>{#each scene.edges as channel (channel.id)}<Mark
          id={channel.id}
          label={`${channel.label}: ${channel.source} → ${channel.target}`}
          selected={selected ?? undefined}
          onselect={interactive ? onselectedchange : undefined}
          ><path
            d={channel.path}
            fill="none"
            stroke={selected === channel.id ? ink : mutedInk}
            stroke-width={selected === channel.id ? 2 : 1.25}
          /><path
            d={channel.arrow}
            fill="none"
            stroke={selected === channel.id ? ink : mutedInk}
          />{#if channel.bus}<text
              x={channel.labelX}
              y={channel.labelY}
              text-anchor="middle"
              fill={mutedInk}
              class={visual.nodeText()}
              >{channel.channel === 0 ? short(channel.bus, 9) : ""}</text
            >{/if}{#if channel.active && playing && allowed}<g
              aria-hidden="true"
              ><animateMotion
                dur="2s"
                repeatCount="indefinite"
                path={channel.path}
              /><circle
                r="3"
                fill={seriesColor(1)}
              />{#if channel.packetLabel}<text
                  y="-8"
                  fill={ink}
                  class={visual.nodeText()}
                  >{short(channel.packetLabel, 12)}</text
                >{/if}</g
            >{/if}</Mark
        >{/each}{#each scene.nodes as node (node.id)}<Mark
          id={node.id}
          label={workflowNodeLabel(node)}
          selected={selected ?? undefined}
          onselect={interactive ? onselectedchange : undefined}
          ><rect
            x={node.x}
            y={node.y}
            width={node.width}
            height={node.height}
            rx={node.paint.radius}
            fill={surface}
            stroke={selected === node.id ? ink : mutedInk}
            stroke-width={selected === node.id ? 2 : 1}
          /><text
            x={node.x + node.paint.inset}
            y={node.y + node.paint.labelY}
            fill={ink}
            class={visual.nodeTitle()}>{node.paint.label}</text
          ><text
            x={node.x + node.paint.inset}
            y={node.y + node.paint.stateY}
            fill={operationalToneInk(node.tone) ?? mutedInk}
            class={visual.nodeText()}>{short(node.state, 24)}</text
          >{#if node.description}<text
              x={node.x + node.paint.inset}
              y={node.y + node.paint.descriptionY}
              fill={mutedInk}
              class={visual.nodeText()}>{node.paint.description}</text
            >{/if}{#if node.percent !== null}<rect
              x={node.x + node.paint.inset}
              y={node.y + node.paint.progressY}
              width={node.width - node.paint.inset * 2}
              height={node.paint.trackHeight}
              fill={edge}
            /><rect
              x={node.x + node.paint.inset}
              y={node.y + node.paint.progressY}
              width={((node.width - node.paint.inset * 2) * node.percent) / 100}
              height={node.paint.trackHeight}
              fill={ink}
            />{/if}{#each node.portPositions as port (port.id)}<circle
              cx={port.x}
              cy={port.y}
              r="3"
              fill={surface}
              stroke={mutedInk}><title>{port.label}</title></circle
            >{/each}</Mark
        >{/each}</svg
    ></SceneViewport
  >
  <ul class={visual.srOnly()} aria-label={`${label} tasks`}>
    {#each data.nodes as node (node.id)}<li>
        {node.label}: {node.state}{node.progress == null
          ? ""
          : `, ${node.progress}%`}
      </li>{/each}
  </ul>
</div>
