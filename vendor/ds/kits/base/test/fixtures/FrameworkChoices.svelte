<script lang="ts">
  import type { Component } from "svelte";
  import Checkbox from "../../src/Checkbox.svelte";
  import Switch from "../../src/Switch.svelte";
  import RadioGroup from "../../src/RadioGroup.svelte";
  import Select from "../../src/Select.svelte";
  let { name, options = {}, rich = false }: { name: string; options?: Record<string, any>; rich?: boolean } = $props();
  const adapters: Record<string, Component<any>> = { Checkbox, Switch, RadioGroup, Select };
  const Adapter = $derived(adapters[name]!);
  export function update(next: Record<string, unknown>) { options = next; }
</script>
<form>
  {#if name === "RadioGroup" && rich}
    <RadioGroup {...options} legend={options.legend} name={options.name} options={options.options}>
      {#snippet optionMedia(option)}<span aria-hidden="true">{option.value}</span>{/snippet}
    </RadioGroup>
  {:else if name === "Select" && rich}
    <Select {...options}><optgroup label="More regions"><option value="eu">Europe</option><option value="ap" disabled>Asia Pacific</option></optgroup></Select>
  {:else}<Adapter {...options} />{/if}
</form>
