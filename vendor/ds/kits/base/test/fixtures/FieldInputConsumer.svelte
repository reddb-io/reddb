<!-- A Product Application's smallest usable form tracer. -->
<script lang="ts">
  import { Field, Input } from "@reddb-io/design-system/base";

  interface Props {
    label?: string;
    help?: string;
    error?: string;
    required?: boolean;
    id?: string;
  }

  const {
    label = "Email address",
    help,
    error,
    required = false,
    id,
  }: Props = $props();
</script>

<Field {label} {help} {error} {required} {id}>
  {#snippet children(control)}
    <Input
      {...control}
      name="email"
      type="email"
      autocomplete="email"
      placeholder="name@example.com"
      data-testid="email"
    />
  {/snippet}
</Field>
