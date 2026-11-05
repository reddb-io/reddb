<script lang="ts">
  import Field from "../../src/Field.svelte";
  import Input from "../../src/Input.svelte";
  import type { FieldContractProps } from "../../src/core/field";
  let { label, help, error, required, id, layout }: FieldContractProps = $props();
  export function clearError() { error = undefined; }
</script>
<Field {label} {help} {error} {required} {id} {layout}>
  {#snippet children(control)}<Input {...control} name="name" />{/snippet}
</Field>
