<!-- Deliberately incomplete range and pressed-state controls. -->
<script lang="ts">
  interface Props {
    failure: "missing-slider-value" | "multi-select-toggle-group";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "missing-slider-value"}
  <div role="slider" aria-label="Deployment capacity" aria-valuemin="0" aria-valuemax="100">
    Capacity
  </div>
{:else}
  <div role="group" aria-label="Alignment">
    <button type="button" aria-pressed="true">Left</button>
    <button type="button" aria-pressed="true">Center</button>
    <button type="button" aria-pressed="false">Right</button>
  </div>
{/if}
