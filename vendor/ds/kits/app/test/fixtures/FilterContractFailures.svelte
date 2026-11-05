<script lang="ts">
  interface Props {
    failure: "active-state-is-only-visual";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "active-state-is-only-visual"}
  <!-- Deliberately broken: visual emphasis never exposes which filter is active. -->
  <div aria-label="Framework" data-broken-filter>
    <button type="button">All</button>
    <button type="button" class="font-medium">Svelte</button>
    <button type="button">React</button>
  </div>
{/if}
