<script lang="ts">
  interface Props {
    failure: "frozen-card-density" | "missing-separator-semantics";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "frozen-card-density"}
  <section data-broken-card class="rounded-lg border p-4">
    Card spacing is fixed instead of following the nearest Density scope.
  </section>
{:else}
  <div data-broken-divider class="border-t"></div>
{/if}
