<script lang="ts">
  import { Card } from "@reddb-io/design-system/base";
</script>

<Card orientation="horizontal" title="Release" description="Ready to ship">
  {#snippet media()}
    <img src="/release.png" alt="Release preview" />
  {/snippet}

  <p>Review the changes.</p>

  {#snippet footer()}
    <button type="button">Publish</button>
  {/snippet}
</Card>
