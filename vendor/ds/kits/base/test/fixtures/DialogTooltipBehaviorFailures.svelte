<!-- Deliberately broken public DOM fixtures for the four behavioral regressions. -->
<script lang="ts">
  interface Props {
    failure: "missing-label" | "ignored-escape" | "lost-focus" | "pointer-only-tooltip";
  }

  const { failure }: Props = $props();
  let open = $state(false);
  let tooltipOpen = $state(false);
</script>

{#if failure === "missing-label"}
  <div role="dialog" aria-modal="true"><h2>Unassociated title</h2></div>
{:else if failure === "ignored-escape"}
  <button type="button" data-opener onclick={() => (open = true)}>Open</button>
  {#if open}<div role="dialog" aria-label="Settings" data-broken-dialog>Still open</div>{/if}
{:else if failure === "lost-focus"}
  <button type="button" data-opener onclick={() => (open = true)}>Open</button>
  {#if open}
    <div role="dialog" aria-label="Members">
      <button type="button" data-close onclick={() => (open = false)}>Close</button>
    </div>
  {/if}
{:else}
  <button
    type="button"
    data-tooltip-trigger
    aria-label="Copy value"
    onmouseenter={() => (tooltipOpen = true)}
    onmouseleave={() => (tooltipOpen = false)}>Copy</button
  >
  {#if tooltipOpen}<span role="tooltip">Copies to the clipboard</span>{/if}
{/if}
