<script lang="ts">
  import { untrack } from "svelte";
  import WorkspaceTabs from "../../src/composites/WorkspaceTabs.svelte";
  let { options }: { options: any } = $props();
  let current = $state(untrack(() => options));
  export function update(next: any) {
    current = next;
  }
</script>

<WorkspaceTabs {...current} />
