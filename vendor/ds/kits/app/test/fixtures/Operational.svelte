<script lang="ts">
  import {
    TaskCard,
    TelemetryRow,
    PropertyPanel,
    PhaseSummary,
    PlaybackControls,
  } from "../../src";
  let { name, options }: { name: string; options: Record<string, any> } =
    $props();
</script>

{#if name === "TaskCard"}<TaskCard
    item={options.item}
    {...options}
  />{:else if name === "TelemetryRow"}<TelemetryRow
    label={options.label}
    value={options.value}
    {...options}
  />{:else if name === "PropertyPanel"}<PropertyPanel
    title={options.title}
    items={options.items}
    {...options}
  />{:else if name === "PhaseSummary"}<PhaseSummary
    label={options.label}
    gates={options.gates}
    {...options}
  />{:else}<PlaybackControls
    label={options.label}
    playing={options.playing}
    position={options.position}
    duration={options.duration}
    {...options}
  />{/if}
