<!-- An ordered pair of canonical DateFields under one native Fieldset. -->
<script lang="ts">
  import type { DateRangeFieldContract } from "./core/contracts/daterangefield";
  import DateField from "./DateField.svelte";
  import Fieldset from "./Fieldset.svelte";
  import { dateRangeField } from "./date-range-field.variants";

  interface Props extends DateRangeFieldContract {
    /** Called with both bounds whenever either changes (ADR 0026's value dialect). */
    onvaluechange?: (value: { start: string; end: string }) => void;
    /** Extra classes merged onto the canonical Fieldset root. */
    class?: string;
  }

  const generatedId = $props.id();
  let {
    label,
    startLabel,
    endLabel,
    startName,
    endName,
    startValue = $bindable(""),
    endValue = $bindable(""),
    onvaluechange,
    help,
    rangeError = "End date cannot be before start date.",
    required = false,
    disabled = false,
    id,
    class: className,
    fieldClass,
    segmentClass,
  }: Props = $props();

  const inverted = $derived(Boolean(startValue && endValue && endValue < startValue));
  const idPrefix = $derived(id ?? `${generatedId}-range`);
  const helpId = $derived(`${idPrefix}-help`);
  const errorId = $derived(`${idPrefix}-error`);
  const describedBy = $derived([help ? helpId : undefined, inverted ? errorId : undefined].filter(Boolean).join(" ") || undefined);
  const styles = dateRangeField();
</script>

<Fieldset
  legend={label}
  {disabled}
  class={styles.root({ class: className })}
  data-date-range-field
  aria-describedby={describedBy}
  aria-invalid={inverted ? "true" : undefined}
>
  <div class={styles.frame()} data-range-field-frame>
    <div class={styles.fields()} data-date-range-fields>
      <DateField
        label={startLabel}
        name={startName}
        bind:value={startValue}
        onvaluechange={(start) => onvaluechange?.({ start, end: endValue })}
        id={`${idPrefix}-start`}
        {required}
        {disabled}
        class={fieldClass}
        {segmentClass}
      />
      <DateField
        label={endLabel}
        name={inverted ? undefined : endName}
        bind:value={endValue}
        onvaluechange={(end) => onvaluechange?.({ start: startValue, end })}
        id={`${idPrefix}-end`}
        {required}
        {disabled}
        class={fieldClass}
        {segmentClass}
      />
    </div>
  </div>
  {#if help}<p class={styles.help()} id={helpId}>{help}</p>{/if}
  {#if inverted}
    <p class={styles.error()} id={errorId} role="alert">{rangeError}</p>
  {/if}
</Fieldset>
