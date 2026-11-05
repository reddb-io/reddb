<!-- A Product Application's Field-bound typeahead at the public Base boundary. -->
<script lang="ts">
  import { Combobox, type ComboboxOption } from "@reddb-io/design-system/base";

  interface Props {
    value?: string;
    required?: boolean;
    error?: string;
    onvaluechange?: (value: string) => void;
    oninputvaluechange?: (value: string) => void;
  }

  let {
    value = $bindable(""),
    required = false,
    error,
    onvaluechange,
    oninputvaluechange,
  }: Props = $props();

  const options: readonly ComboboxOption[] = [
    { value: "us-east-1", label: "North America" },
    { value: "sa-east-1", label: "South America" },
    { value: "eu-west-1", label: "Europe", disabled: true },
  ];
</script>

<form>
  <div data-theme="nested" data-color-scheme="dark" data-density="compact" data-appearance-scope>
    <Combobox
      label="Region"
      help="Type to narrow the available regions."
      {error}
      {required}
      name="region"
      {options}
      bind:value
      {onvaluechange}
      {oninputvaluechange}
      class="max-w-md"
    />
  </div>
</form>
