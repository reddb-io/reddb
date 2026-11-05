<script lang="ts">
  import NumberField from "../../src/NumberField.svelte";
  import Slider from "../../src/Slider.svelte";
  import OneTimeCodeInput from "../../src/OneTimeCodeInput.svelte";
  let { name, options = {}, changed = () => {}, complete = () => {} }: { name: string; options?: Record<string, any>; changed?: (value: any) => void; complete?: (value: string) => void } = $props();
  export function update(next: Record<string, unknown>) { options = next; }
</script>
<form id="numeric-form">
  {#if name === "NumberField"}<NumberField {...options} label={options.label} onvaluechange={changed} />
  {:else if name === "Slider"}<Slider {...options} label={options.label} onvaluechange={changed} />
  {:else}<OneTimeCodeInput {...options} label={options.label} onvaluechange={changed} oncomplete={complete} />{/if}
</form>
