<!-- Status vocabulary at the public Base package boundary, nested below an
     appearance scope owned entirely by the consumer. -->
<script lang="ts">
  import { Badge, Indicator, StatusIndicator } from "@reddb-io/design-system/base";
</script>

<div
  data-appearance-scope
  data-theme="marketing"
  data-color-scheme="dark"
  data-density="compact"
>
  <Badge tone="success">Stable</Badge>
  <StatusIndicator label="Available" tone="success" />
  <Indicator position="top-end">
    {#snippet marker()}<StatusIndicator label="Delayed" tone="warning" showLabel={false} />{/snippet}
    <a href="/service">Service</a>
  </Indicator>
</div>
