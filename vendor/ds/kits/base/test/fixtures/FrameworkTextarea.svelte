<script lang="ts">
  import { untrack } from "svelte";
  import Textarea from "../../src/Textarea.svelte";
  const { initialValue, onValueChange }: { initialValue: string; onValueChange: (value: string) => void } = $props();
  let value = $state(untrack(() => initialValue));
  export function setValue(next: string) { value = next; }
</script>
<Textarea bind:value onvaluechange={onValueChange} name="notes" aria-label="Notes" />
