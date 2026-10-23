<script lang="ts">
  interface Props {
    failure: "no-roving-focus" | "escape-survives";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "no-roving-focus"}
  <div role="menu" aria-label="Broken actions" data-broken-menu>
    <button type="button" role="menuitem">Rename</button>
    <button type="button" role="menuitem">Duplicate</button>
  </div>
{:else}
  <button type="button" aria-haspopup="menu" aria-expanded="true">Actions</button>
  <div role="menu" aria-label="Broken actions" data-broken-menu>
    <button type="button" role="menuitem">Rename</button>
  </div>
{/if}
