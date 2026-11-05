<!-- Deliberately broken feedback surfaces. The notification is visible but
     never enters an assistive-technology live region; the empty region offers
     an action without saying what is empty. -->
<script lang="ts">
  interface Props {
    failure: "silent-notification" | "unnamed-empty-state";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "silent-notification"}
  <div data-broken-notification>
    <p>Deployment complete</p>
    <button type="button">Dismiss</button>
  </div>
{:else}
  <div data-broken-empty-state>
    <button type="button">Add a node</button>
  </div>
{/if}
