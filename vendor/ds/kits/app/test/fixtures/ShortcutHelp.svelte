<script lang="ts">
  import { untrack } from "svelte";
  import ShortcutHelp from "../../src/composites/ShortcutHelp.svelte";
  let { options }: { options: any } = $props();
  let current = $state(untrack(() => options));
  export function update(next: any) {
    current = next;
  }
</script>

<ShortcutHelp {...current} />
