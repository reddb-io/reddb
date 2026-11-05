<!-- A Product Application's native multiline and choice controls, composed
     through Field at the public Base package boundary. -->
<script lang="ts">
  import { Field, Select, Textarea } from "@reddb-io/design-system/base";

  interface Props {
    error?: string;
    required?: boolean;
  }

  const { error, required = false }: Props = $props();
</script>

<form>
  <Field label="Deployment notes" help="Include rollback details." {error} {required}>
    {#snippet children(control)}
      <Textarea {...control} name="notes" rows={4} value="Preserve the current release." />
    {/snippet}
  </Field>

  <Field label="Region" {required}>
    {#snippet children(control)}
      <Select {...control} name="region" value="sa-east-1">
        <option value="">Choose a region</option>
        <option value="us-east-1">North America</option>
        <option value="sa-east-1">South America</option>
      </Select>
    {/snippet}
  </Field>
</form>
