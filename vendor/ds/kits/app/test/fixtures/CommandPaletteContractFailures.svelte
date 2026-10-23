<script lang="ts">
  interface Props {
    failure: "focus-not-restored";
  }

  const { failure }: Props = $props();
  let open = $state(false);
</script>

{#if failure === "focus-not-restored"}
  <button type="button" data-broken-palette-trigger onclick={() => (open = true)}>
    Search commands
  </button>
  {#if open}
    <div role="dialog" aria-label="Commands">
      <input aria-label="Command" />
      <button type="button" data-broken-command onclick={() => (open = false)}>Open settings</button>
    </div>
  {/if}
{/if}
