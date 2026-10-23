<script lang="ts">
  import InlineEdit from "../../src/InlineEdit.svelte";
  import Reveal from "../../src/Reveal.svelte";
  let { name, options = {}, changed = () => {}, editingChanged = () => {} }: { name: string; options?: Record<string, any>; changed?: (value: string) => void; editingChanged?: (next: boolean) => void } = $props();
  export function update(next: Record<string, any>) { options = next; }
</script>
{#if name === "InlineEdit"}
  <InlineEdit {...options} label={options.label} value={options.value}
    onvaluechange={value => { changed(value); options = { ...options, value }; }}
    oneditingchange={editing => { editingChanged(editing); if (options.editing !== undefined) options = { ...options, editing }; }} />
{:else}<Reveal {...options}><p>Review details</p></Reveal>{/if}
