<!-- The three capabilities composed at the public Base package boundary,
     including one nested appearance scope owned entirely by the consumer. -->
<script lang="ts">
  import {
    Button,
    ControlGroup,
    FileInput,
    OneTimeCodeInput,
  } from "@reddb-io/design-system/base";

  interface Props {
    fileOnchange?: (event: Event) => void;
    otpOncomplete?: (value: string) => void;
  }

  const { fileOnchange, otpOncomplete }: Props = $props();
</script>

<form>
  <FileInput
    label="Upload evidence"
    help="PDF or image, up to 10 MB."
    name="evidence"
    accept=".pdf,image/*"
    onchange={fileOnchange}
    required
  />

  <OneTimeCodeInput
    label="Verification code"
    help="Paste or enter the six-digit code."
    name="verificationCode"
    length={6}
    oncomplete={otpOncomplete}
    required
  />

  <ControlGroup label="Record actions">
    <Button type="button">Save</Button>
    <Button type="button" variant="secondary">Cancel</Button>
  </ControlGroup>

  <div
    data-appearance-scope
    data-theme="marketing"
    data-color-scheme="dark"
    data-density="compact"
  >
    <FileInput label="Nested upload" name="nested-file" />
    <OneTimeCodeInput label="Nested code" name="nested-code" length={4} />
    <ControlGroup label="Nested actions" orientation="vertical">
      <Button type="button">Continue</Button>
      <Button type="button" variant="secondary">Back</Button>
    </ControlGroup>
  </div>
</form>
