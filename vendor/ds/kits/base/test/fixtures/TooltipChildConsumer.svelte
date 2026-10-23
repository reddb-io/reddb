<!-- A Tooltip decorating a trigger the consumer already renders. -->
<script lang="ts">
  import { Tooltip } from "@reddb-io/design-system/base";
</script>

<Tooltip content="Copies the connection string">
  {#snippet child({ props })}
    <a {...props} href="#copy" data-own-trigger>Copy</a>
  {/snippet}
</Tooltip>
