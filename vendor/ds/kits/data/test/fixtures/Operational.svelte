<script lang="ts">
  import StateGrid from "../../src/StateGrid.svelte";
  import EventLog from "../../src/EventLog.svelte";
  import QueueView from "../../src/QueueView.svelte";
  import IsometricBlock from "../../src/IsometricBlock.svelte";
  import ObjectStack from "../../src/ObjectStack.svelte";
  import IsometricScene from "../../src/IsometricScene.svelte";
  import SceneViewport from "../../src/SceneViewport.svelte";
  import WorkflowDiagram from "../../src/WorkflowDiagram.svelte";
  import MetricStrip from "../../src/MetricStrip.svelte";
  let { name, options }: { name: string; options: Record<string, any> } =
    $props();
</script>

{#if name === "StateGrid"}<StateGrid
    label={options.label}
    items={options.items}
    {...options}
  />{:else if name === "QueueView"}<QueueView
    label={options.label}
    slots={options.slots}
    {...options}
  />{:else if name === "IsometricBlock"}<IsometricBlock
    label={options.label}
    {...options}
  />{:else if name === "ObjectStack"}<ObjectStack
    label={options.label}
    count={options.count}
    {...options}
  />{:else if name === "IsometricScene"}<IsometricScene
    label={options.label}
    items={options.items}
    {...options}
  />{:else if name === "WorkflowDiagram"}<WorkflowDiagram
    label={options.label}
    data={options.data}
    {...options}
  />{:else if name === "EventLog"}<EventLog
    label={options.label}
    events={options.events}
    {...options}
  />{:else if name === "MetricStrip"}<MetricStrip
    label={options.label}
    items={options.items}
    {...options}
  />{:else}<SceneViewport
    label={options.label}
    contentWidth={options.contentWidth}
    contentHeight={options.contentHeight}
    {...options}><p>Static scene content</p></SceneViewport
  >{/if}
