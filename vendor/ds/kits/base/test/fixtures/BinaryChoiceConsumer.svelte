<script lang="ts">
  import { Checkbox, Form, RadioGroup, Switch } from "@reddb-io/design-system/base";

  interface Props {
    checkboxChecked?: boolean;
    checkboxOnchange?: (event: Event) => void;
    radioOnkeydown?: (event: KeyboardEvent) => void;
    switchOnchange?: (event: Event) => void;
  }

  const {
    checkboxChecked = true,
    checkboxOnchange,
    radioOnkeydown,
    switchOnchange,
  }: Props = $props();

  const channels = [
    { value: "stable", label: "Stable" },
    { value: "preview", label: "Preview" },
  ] as const;
</script>

<Form data-testid="binary-choice-form">
  <Checkbox
    label="Share diagnostics"
    name="telemetry"
    value="allowed"
    checked={checkboxChecked}
    onchange={checkboxOnchange}
  />

  <RadioGroup
    legend="Release channel"
    name="channel"
    options={channels}
    value="stable"
    onkeydown={radioOnkeydown}
  />

  <Switch
    label="Automatic updates"
    name="updates"
    value="automatic"
    onchange={switchOnchange}
  />

  <div
    data-appearance-scope
    data-theme="marketing"
    data-color-scheme="dark"
    data-density="compact"
  >
    <Checkbox label="Nested diagnostics" name="nested-diagnostics" />
    <RadioGroup
      legend="Nested channel"
      name="nested-channel"
      options={channels}
      value="preview"
    />
    <Switch label="Nested updates" name="nested-updates" />
  </div>
</Form>
