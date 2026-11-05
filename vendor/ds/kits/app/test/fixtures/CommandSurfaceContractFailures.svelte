<script lang="ts">
  interface Props {
    failure: "pointer-only-context-menu" | "toolbar-without-roving-focus";
  }

  let { failure }: Props = $props();
</script>

{#if failure === "pointer-only-context-menu"}
  <div
    data-context-target
    role="region"
    aria-label="Pointer-only context target"
    oncontextmenu={(event) => event.preventDefault()}
  >
    Right-click for actions
  </div>
{:else}
  <div role="toolbar" aria-label="Formatting">
    <button type="button">Bold</button>
    <button type="button">Italic</button>
    <button type="button">Underline</button>
  </div>
{/if}
