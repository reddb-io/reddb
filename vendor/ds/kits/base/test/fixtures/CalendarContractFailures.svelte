<script lang="ts">
  interface Props {
    failure: "calendar-without-keyboard-navigation" | "range-without-announcement";
  }

  let { failure }: Props = $props();
</script>

{#if failure === "calendar-without-keyboard-navigation"}
  <div role="grid" aria-label="Deployment date">
    <div role="row">
      <button type="button" role="gridcell" tabindex="-1">1</button>
      <button type="button" role="gridcell" tabindex="-1">2</button>
    </div>
  </div>
{/if}

{#if failure === "range-without-announcement"}
  <div role="application" aria-label="Deployment window June 2026">
    <div role="grid">
      <div role="row">
        <button type="button" role="gridcell" aria-selected="true">10</button>
        <button type="button" role="gridcell" aria-selected="true">12</button>
      </div>
    </div>
  </div>
{/if}
