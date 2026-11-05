<script lang="ts">
  import { untrack } from "svelte";
  import type { HTMLInputAttributes } from "svelte/elements";
  import Button from "./Button.svelte";
  import Field from "./Field.svelte";
  import Input from "./Input.svelte";
  import type { InputSize } from "./input.variants";
  import { numberField } from "./number-field.variants";
  import { numberConstraints } from "./core/number-field";
  import { afterFormReset, associatedForm } from "./core/native-controls";

  interface Props extends Omit<HTMLInputAttributes, "type" | "value" | "min" | "max" | "step" | "class" | "children" | "size" | "defaultValue"> {
    /** Visible label, also used to name both stepping actions. */
    label: string;
    /** Current number; null represents an empty field. Bindable. Values outside the constraints retain native validation errors. */
    value?: number | null;
    /** Initial uncontrolled value, restored by native form reset. */
    defaultValue?: number | null;
    /** Called once for each user edit or step, and after a native form reset. */
    onvaluechange?: (value: number | null) => void;
    /** Finite inclusive lower bound, if any. */
    min?: number;
    /** Finite inclusive upper bound, if any; must not be below min. */
    max?: number;
    /** Positive finite native step, based on min or the initial value. Defaults to 1. */
    step?: number;
    /** Supporting text associated with the native input. */
    help?: string;
    /** Application validation message associated with the native input. */
    error?: string;
    /** Shared control height for the input and both buttons. Defaults to md. */
    controlSize?: InputSize;
    /** Accessible decrement action name; defaults to Decrease followed by label. */
    decrementLabel?: string;
    /** Accessible increment action name; defaults to Increase followed by label. */
    incrementLabel?: string;
    /** Extra classes on the Field root. */
    class?: string;
  }

  let {
    label, value = $bindable(), defaultValue = null, onvaluechange, min, max, step = 1,
    help, error, controlSize = "md", decrementLabel, incrementLabel,
    id, required = false, disabled = false, readonly = false,
    class: className, ...rest
  }: Props = $props();
  let input = $state<HTMLInputElement>();
  const initialValue = untrack(() => value === undefined ? defaultValue : value);
  let local = $state(initialValue);
  const current = $derived(value === undefined ? local : value);
  const constraints = $derived(numberConstraints(min, max, step));
  const slots = numberField();
  const atMinimum = $derived(current !== null && min !== undefined && current <= min);
  const atMaximum = $derived(current !== null && max !== undefined && current >= max);

  $effect(() => {
    // Input preserves uncontrolled fields. Here null is an explicit controlled empty value.
    if (input && current === null && input.value !== "") input.value = "";
  });

  function update(next: HTMLInputAttributes["value"]): void {
    const normalized = typeof next === "number" && Number.isFinite(next) ? next : null;
    local = normalized; value = normalized;
    onvaluechange?.(normalized);
  }

  function move(direction: -1 | 1): void {
    if (!input || input.disabled || input.matches(":disabled") || readonly) return;
    const before = input.value;
    // The platform owns step alignment and decimal arithmetic, just as with arrow keys.
    if (direction === 1) input.stepUp(); else input.stepDown();
    if (before === input.value) return;
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("change", { bubbles: true }));
  }

  $effect(() => {
    const element = input;
    if (!element) return;
    // Svelte removes SSR defaults during hydration. Restore the initial native
    // step base after that cleanup, preserving decimal offsets and validity.
    queueMicrotask(() => {
      if (element?.isConnected) element.defaultValue = initialValue === null ? "" : String(initialValue);
    });
    return afterFormReset(associatedForm(element, rest.form), () => {
      if (!element.isConnected) return;
      element.value = initialValue === null ? "" : String(initialValue);
      update(initialValue);
    });
  });
</script>

<Field {label} {help} {error} required={!!required} id={id ?? undefined} class={className}>
  {#snippet children(control)}
    <div data-number-field class={slots.root()}>
      <Button size={controlSize} variant="secondary" class={slots.step()} aria-label={decrementLabel ?? `Decrease ${label}`}
        disabled={disabled || readonly || atMinimum} onclick={() => move(-1)}>−</Button>
      <Input {...rest} {...control} type="number" value={current} {...constraints} {disabled} {readonly} {controlSize}
        bind:ref={input} onvaluechange={update} class={slots.input()}/>
      <Button size={controlSize} variant="secondary" class={slots.step()} aria-label={incrementLabel ?? `Increase ${label}`}
        disabled={disabled || readonly || atMaximum} onclick={() => move(1)}>+</Button>
    </div>
  {/snippet}
</Field>
