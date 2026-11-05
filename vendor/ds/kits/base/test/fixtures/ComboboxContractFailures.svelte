<!-- Deliberately broken public DOM fixtures for Combobox regressions. -->
<script lang="ts">
  interface Props {
    failure: "missing-active-descendant" | "discarded-input";
  }

  const { failure }: Props = $props();
  let typed = $state("");
</script>

{#if failure === "missing-active-descendant"}
  <input role="combobox" aria-expanded="true" aria-controls="broken-options" />
  <div id="broken-options" role="listbox">
    <div id="broken-option" role="option" aria-selected="false">South America</div>
  </div>
{:else}
  <input
    role="combobox"
    aria-controls="unused-options"
    aria-expanded="false"
    value={typed}
    oninput={(event) => {
      typed = "";
      event.currentTarget.value = typed;
    }}
  />
{/if}
