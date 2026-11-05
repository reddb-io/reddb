<script lang="ts">
  import { AlertDialog, Drawer } from "@reddb-io/design-system/base";
</script>

<section
  data-overlay-appearance-scope
  data-theme="marketing"
  data-color-scheme="dark"
  data-density="compact"
>
  <Drawer triggerLabel="Open filters" title="Filters" data-nested-drawer />
  <AlertDialog
    triggerLabel="Delete deployment"
    title="Delete deployment?"
    confirmLabel="Delete"
    data-nested-alert-dialog
  />
</section>
