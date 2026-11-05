<!-- Deliberately broken public DOM fixtures for overlay regressions. -->
<script lang="ts">
  interface Props {
    failure: "drawer-lost-focus" | "alert-outside-dismissal";
  }

  const { failure }: Props = $props();
  let open = $state(false);
</script>

<button type="button" data-opener onclick={() => (open = true)}>Open</button>

{#if open && failure === "drawer-lost-focus"}
  <div role="dialog" aria-modal="true" aria-label="Filters" data-broken-drawer>
    <button type="button" data-close onclick={() => (open = false)}>Close</button>
  </div>
{:else if open}
  <!-- svelte-ignore a11y_interactive_supports_focus -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    role="alertdialog"
    aria-modal="true"
    aria-label="Delete deployment"
    data-broken-alert-dialog
    onclick={() => (open = false)}
  >
    <button type="button">Cancel</button>
    <button type="button">Delete</button>
  </div>
{/if}
