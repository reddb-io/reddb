<!--
  Field — the accessible contract around one form control.

  Field owns the relationships: its label targets the control id, help and
  error text describe it, an error marks it invalid, and required reaches both
  the visible label and the native control. The child snippet receives that
  complete contract, which keeps Input native and lets a consumer substitute a
  local control without reconstructing accessibility ids.
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import { field, type FieldControlProps } from "./field.variants";
  import { fieldAssociation, type FieldContractProps } from "./core/field";
  import Label from "./Label.svelte";
  import { setFieldContext, warnA11y } from "./a11y";

  interface Props extends FieldContractProps {
    /** Extra classes on the Field root. */
    class?: string;
    /** The canonical Input or a consumer-owned control. */
    children: Snippet<[FieldControlProps]>;
  }

  const generatedId = $props.id();
  const {
    label,
    help,
    error,
    required = false,
    id,
    layout = "stacked",
    class: className,
    children,
  }: Props = $props();

  const controlId = $derived(id ?? `${generatedId}-control`);
  const association = $derived(fieldAssociation({ id: controlId, help, error, required }));
  const helpId = $derived(association.helpId);
  const errorId = $derived(association.errorId);
  const control = $derived(association.control);
  const styles = $derived(field({ layout }));

  // The control inside reads this to know its name is owned here, through the
  // label's `for` (wave 5A's accessible-name contract).
  setFieldContext({
    get label() {
      return label;
    },
  });
  $effect(() => {
    if (!label?.trim()) warnA11y("Field", "has an empty `label`, so its control has no accessible name", "Give the Field a visible `label`.");
  });
</script>

<div
  class={styles.root({ class: className })}
  data-field-layout={layout}
  data-field-invalid={error ? "true" : undefined}
>
  {#if layout === "inline"}{@render children(control)}{/if}

  <Label class={styles.label()} for={controlId}>
    {label}{#if required}<span class={styles.required()} aria-hidden="true">*</span><span class="sr-only"> (required)</span>{/if}
  </Label>

  {#if layout !== "inline"}{@render children(control)}{/if}

  {#if help}
    <p class={styles.help()} id={helpId}>{help}</p>
  {/if}

  {#if error}
    <p class={styles.error()} id={errorId} role="alert">{error}</p>
  {/if}
</div>
