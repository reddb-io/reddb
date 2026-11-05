<script lang="ts">
  import { Button, Input } from "@reddb-io/design-system/base";
  import SettingsRow from "../../src/composites/SettingsRow.svelte";
</script>
<SettingsRow label="Signing secret" description="At least 16 characters." id="secret">
  {#snippet children(control)}<Input {...control} type="password" />{/snippet}
</SettingsRow>
<SettingsRow label="HTTPS and mobile access" description="Connect another device.">
  {#snippet actions()}<Button>Configure HTTPS</Button>{/snippet}
</SettingsRow>
