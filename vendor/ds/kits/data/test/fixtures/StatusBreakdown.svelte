<script lang="ts">
  import StatusBreakdown from "../../src/StatusBreakdown.svelte";
  let { options }: { options: Record<string, any> } = $props();
  let current = $state((() => options)());
  export function update(next: Record<string, any>) { current = next; }
</script>
<StatusBreakdown {...(current as any)} />
