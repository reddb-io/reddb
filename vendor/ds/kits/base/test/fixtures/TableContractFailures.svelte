<script lang="ts">
  let { failure }: { failure: "missing-header-association" | "silent-overflow" } = $props();
</script>

{#if failure === "missing-header-association"}
  <!-- Bold cells look like headings, but no native header association exists. -->
  <table data-broken-table>
    <tbody>
      <tr><td><strong>Region</strong></td><td><strong>Latency</strong></td></tr>
      <tr><td>South America</td><td>24 ms</td></tr>
    </tbody>
  </table>
{:else}
  <!-- The wide table is clipped without exposing a keyboard-scrollable region. -->
  <div data-broken-table-overflow class="overflow-hidden">
    <table style="min-width: 64rem">
      <caption>Cluster replicas</caption>
      <thead><tr><th scope="col">Node</th><th scope="col">Region</th></tr></thead>
      <tbody><tr><th scope="row">db-01</th><td>South America</td></tr></tbody>
    </table>
  </div>
{/if}
