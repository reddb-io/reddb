<script lang="ts">
  let { failure }: { failure: "missing-list-semantics" | "broken-term-detail-pairing" } = $props();
</script>

{#if failure === "missing-list-semantics"}
  <div data-broken-list>
    <div>First result</div>
    <div>Second result</div>
  </div>
{:else}
  <dl data-broken-description-list>
    <dt>Region</dt>
    <dt>Owner</dt>
    <dd>South America</dd>
  </dl>
{/if}
