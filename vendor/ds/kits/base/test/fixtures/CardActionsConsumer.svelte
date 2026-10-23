<script lang="ts">
  import { Card } from "@reddb-io/design-system/base";

  let { actionsAlign }: { actionsAlign: "start" | "center" | "end" | "between" } = $props();
</script>

<Card {actionsAlign}>
  {#snippet footer()}
    <button type="button">Cancel</button>
    <button type="button">Save</button>
  {/snippet}
</Card>
