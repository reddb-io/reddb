<!-- Both feedback surfaces through the distributed Base subpath, nested below
     appearance axes selected entirely by the consumer. -->
<script lang="ts">
  import { Button, EmptyState, Notification } from "@reddb-io/design-system/base";
</script>

<div
  data-appearance-scope
  data-theme="marketing"
  data-color-scheme="dark"
  data-density="compact"
>
  <div data-nested-notification>
    <Notification feedback="warning" title="Capacity is nearly full">
      Add capacity before the next deployment.
    </Notification>
  </div>

  <div data-nested-empty-state>
    <EmptyState
      title="No nodes yet"
      description="A node appears here as soon as one joins."
      hint="reddb node add --name alpha"
    >
      {#snippet actions()}<Button>Add a node</Button>{/snippet}
    </EmptyState>
  </div>
</div>
