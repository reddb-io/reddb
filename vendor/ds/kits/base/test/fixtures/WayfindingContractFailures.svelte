<script lang="ts">
  let { failure }: { failure: "breadcrumbs-current" | "pagination-labels" } = $props();
</script>

{#if failure === "breadcrumbs-current"}
  <nav aria-label="Breadcrumbs" data-broken-breadcrumbs>
    <ol>
      <li><a href="/clusters">Clusters</a></li>
      <li><a href="/clusters/primary">Primary</a></li>
    </ol>
  </nav>
{:else}
  <nav aria-label="Pagination" data-broken-pagination>
    <a href="?page=1">1</a>
    <a href="?page=2">2</a>
  </nav>
{/if}
