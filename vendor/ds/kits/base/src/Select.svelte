<!--
  Select — the canonical Base choice control.

  The options belong to the caller, while the platform keeps the popup,
  keyboard interaction, focus, selection, validation and form semantics.
-->
<script module lang="ts">
  export type { SelectOption } from "./core/choices";
</script>

<script lang="ts">
  import { untrack } from "svelte";
  import { afterFormReset } from "./core/native-controls";
  import { readSelectValue, type SelectOption } from "./core/choices";
  import { checkFieldName, getFieldContext } from "./a11y";
  import type { Snippet } from "svelte";
  import type { HTMLSelectAttributes } from "svelte/elements";
  import { select, type SelectSize } from "./select.variants";

  interface Props extends Omit<HTMLSelectAttributes, "children" | "class" | "value"> {
    /** The native option and optgroup content. */
    children?: Snippet;
    /** Flat caller-owned choices for compositions that do not need custom option markup. */
    options?: readonly SelectOption[];
    /** The native select, available for imperative focus. */
    ref?: HTMLSelectElement;
    /** Current native value, bindable for canonical compositions. */
    value?: HTMLSelectAttributes["value"];
    /** Initial uncontrolled native value, including arrays for multiple selection. */
    defaultValue?: HTMLSelectAttributes["value"];
    /** Called with the newly selected value (ADR 0026's value dialect); the native `onchange` still receives the Event. */
    onvaluechange?: (value: string) => void;
    /**
     * The control height step, matching Button's `size` (default `md`). Named
     * apart from the native `size` attribute, which is still spread through.
     */
    controlSize?: SelectSize;
    /** Extra classes, merged over the canonical appearance. */
    class?: string;
  }

  let {
    ref = $bindable(),
    value = $bindable(),
    defaultValue,
    onvaluechange,
    onchange,
    children,
    options = [],
    controlSize = "md",
    class: className,
    ...rest
  }: Props = $props();

  const initial = untrack(() => value ?? defaultValue);
  let local = $state(initial);
  let element = $state<HTMLSelectElement>();
  $effect(() => { if (ref !== element) ref = element; });
  $effect(() => {
    options; children;
    if (!element || initial === undefined) return;
    const selected = new Set((Array.isArray(initial) ? initial : [initial]).map(String));
    // Native reset uses defaultSelected, including for caller-owned optgroups.
    for (const option of element.options) option.defaultSelected = selected.has(option.value);
  });
  $effect(() => {
    rest.form;
    return afterFormReset(element?.form, () => {
      if (element) { local = readSelectValue(element); value = local; }
    });
  });

  // Wave 5A's accessible-name contract: inside a Field the label owns the
  // name; anywhere else a mounted control with no name warns in development.
  const insideField = getFieldContext() !== undefined;
  $effect(() => {
    if (!insideField) checkFieldName("Select", ref);
  });
</script>

<select
  {...rest}
  bind:this={element}
  bind:value={() => value ?? local, next => { local = next; value = next; }}
  onchange={(event) => {
    const next = event.currentTarget.value;
    onchange?.(event);
    onvaluechange?.(next);
  }}
  class={select({ size: controlSize, class: className })}>
  {#each options as option (option.value)}
    <option value={option.value} disabled={option.disabled}>{option.label}</option>
  {/each}
  {@render children?.()}
</select>
