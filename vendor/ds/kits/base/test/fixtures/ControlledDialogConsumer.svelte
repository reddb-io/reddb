<!-- A Dialog opened from outside — a menu item, a command, a route — through bind:open. -->
<script lang="ts">
  import { AlertDialog, Dialog } from "@reddb-io/design-system/base";

  interface Props {
    onconfirm?: () => void | Promise<unknown>;
  }

  let { onconfirm }: Props = $props();
  let settings = $state(false);
  let confirm = $state(false);
  let changes: boolean[] = $state([]);
</script>

<button type="button" data-open-settings onclick={() => (settings = true)}>Settings</button>
<button type="button" data-open-confirm onclick={() => (confirm = true)}>Delete</button>
<output data-settings-open>{String(settings)}</output>
<output data-changes>{changes.join(",")}</output>

<Dialog
  title="Settings"
  bind:open={settings}
  onopenchange={(open) => changes.push(open)}
  data-settings-dialog=""
>
  <p>Body</p>
</Dialog>

<AlertDialog title="Delete deployment?" confirmLabel="Delete" bind:open={confirm} {onconfirm} />
