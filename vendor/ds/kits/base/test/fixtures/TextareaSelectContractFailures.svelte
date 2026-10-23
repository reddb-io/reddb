<!-- Deliberately incomplete controls: proof that the suites diagnose the
     multiline and keyboard-select gaps rather than merely rendering markup. -->
<script lang="ts">
  interface Props {
    failure: "single-line" | "mouse-only-select";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "single-line"}
  <input name="notes" aria-label="Notes" />
{:else}
  <div data-mouse-only-select>
    <button type="button" tabindex="-1">Choose a region</button>
    <div>North America</div>
    <div>South America</div>
  </div>
{/if}
