<script lang="ts">
  import EntityInspector from "../../src/composites/EntityInspector.svelte";
  let { options }: { options: Record<string, any> } = $props();
  let current = $state((() => options)());
  export function update(next: Record<string, any>) { current = next; }
</script>
<EntityInspector {...(current as any)}><p>Caller-owned details</p>{#snippet actions()}<span>Review controls</span>{/snippet}</EntityInspector>
