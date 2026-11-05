<!-- Deliberately incomplete disclosure-family controls used to prove the diagnostics. -->
<script lang="ts">
  interface Props {
    failure: "tabs-without-arrow-navigation" | "accordion-without-expanded-state";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "tabs-without-arrow-navigation"}
  <div role="tablist" aria-label="Deployment view">
    <button type="button" role="tab" aria-selected="true" tabindex="0">Summary</button>
    <button type="button" role="tab" aria-selected="false" tabindex="-1">Events</button>
  </div>
  <div role="tabpanel">Summary content</div>
{:else}
  <section data-accordion>
    <h3><button type="button">What changed?</button></h3>
    <div>Release notes</div>
  </section>
{/if}
