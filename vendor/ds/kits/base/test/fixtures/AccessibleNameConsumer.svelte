<!-- Wave 5A: every way a field and a Button get their accessible name, and the ways they do not. -->
<script lang="ts">
  import { Button, Field, Input, Select, Textarea } from "@reddb-io/design-system/base";

  interface Props {
    case:
      | "field"
      | "aria-label"
      | "aria-labelledby"
      | "label-for"
      | "wrapping-label"
      | "unnamed-input"
      | "unnamed-textarea"
      | "unnamed-select"
      | "empty-field"
      | "text-button"
      | "sr-only-button"
      | "icon-button"
      | "labelled-icon-button";
  }

  const { case: which }: Props = $props();
</script>

{#if which === "field"}
  <Field label="Name">
    {#snippet children(control)}<Input {...control} />{/snippet}
  </Field>
  <Field label="Notes">
    {#snippet children(control)}<Textarea {...control} />{/snippet}
  </Field>
  <Field label="Region">
    {#snippet children(control)}<Select {...control} options={[{ value: "a", label: "A" }]} />{/snippet}
  </Field>
{:else if which === "aria-label"}
  <Input aria-label="Search" />
  <Textarea aria-label="Notes" />
  <Select aria-label="Region"><option>A</option></Select>
{:else if which === "aria-labelledby"}
  <span id="name-label">Name</span>
  <Input aria-labelledby="name-label" />
{:else if which === "label-for"}
  <label for="native-name">Name</label>
  <Input id="native-name" />
{:else if which === "wrapping-label"}
  <label>Name <Input /></label>
{:else if which === "unnamed-input"}
  <Input placeholder="Placeholder is not a name" />
{:else if which === "unnamed-textarea"}
  <Textarea />
{:else if which === "unnamed-select"}
  <Select><option>A</option></Select>
{:else if which === "empty-field"}
  <Field label="">
    {#snippet children(control)}<Input {...control} />{/snippet}
  </Field>
{:else if which === "text-button"}
  <Button>Save</Button>
{:else if which === "sr-only-button"}
  <Button><svg aria-hidden="true"></svg><span class="sr-only">Close</span></Button>
{:else if which === "icon-button"}
  <Button><svg aria-hidden="true"></svg></Button>
{:else if which === "labelled-icon-button"}
  <Button aria-label="Close"><svg aria-hidden="true"></svg></Button>
{/if}
