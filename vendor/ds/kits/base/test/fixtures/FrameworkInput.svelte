<script lang="ts">
  import { untrack } from "svelte";
  import Input from "../../src/Input.svelte";
  let { type, initialValue, onValueChange }: { type: "text" | "number" | "range"; initialValue: string | number | null; onValueChange: (value: unknown) => void } = $props();
  let value = $state(untrack(() => initialValue));
  export function setValue(next: string | number | null) { value = next; }
</script>
<Input {type} bind:value min="0" max="10" step="any" onvaluechange={onValueChange} aria-label="Value" name="value" />
