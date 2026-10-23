<script lang="ts">
  import { Fieldset, Form, Input, Label } from "@reddb-io/design-system/base";

  interface Props {
    disabledGroup?: boolean;
    onsubmit?: (event: SubmitEvent) => void;
  }

  const { disabledGroup = false, onsubmit }: Props = $props();
</script>

<div>
  <Form
    action="/profiles"
    method="post"
    autocomplete="off"
    novalidate
    class="max-w-xl"
    data-testid="profile-form"
    {onsubmit}
  >
    <Label
      for="display-name"
      title="Your public name"
      class="tracking-wide"
      data-testid="display-name-label"
    >Display name</Label>
    <Input id="display-name" name="displayName" />

    <Fieldset
      legend="Contact preferences"
      disabled={disabledGroup}
      class="min-w-0"
      data-intended-group
    >
      <Label for="email-notices">Email notices</Label>
      <Input id="email-notices" name="notices" type="checkbox" value="email" />
    </Fieldset>

    <button type="submit">Save profile</button>
  </Form>

  <div
    data-appearance-scope
    data-theme="marketing"
    data-color-scheme="dark"
    data-density="compact"
  >
    <Form data-testid="nested-form">
      <Label for="nested-query" data-testid="nested-label">Search</Label>
      <Input id="nested-query" name="query" type="search" />
      <Fieldset legend="Search options"><p>No filters selected.</p></Fieldset>
    </Form>
  </div>
</div>
