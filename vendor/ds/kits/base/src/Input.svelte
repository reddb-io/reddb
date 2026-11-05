<!--
  Input — the canonical Base text entry.

  It stays a native <input>: the complete platform attribute surface is spread
  through, including identity, autofill, constraints, validation, events and
  the native `size` attribute. `value` is bindable (`bind:value`), and `ref`
  is bindable for the rare caller that must move focus imperatively; ordinary
  focus remains the browser's own behavior.
-->
<script lang="ts">
  import { checkFieldName, getFieldContext } from "./a11y";
  import { untrack } from "svelte";
  import type { HTMLInputAttributes } from "svelte/elements";
  import { input, type InputSize } from "./input.variants";
  import { inputValue } from "./core/input";

  interface Props extends Omit<HTMLInputAttributes, "class" | "value"> {
    /** The current value. Bindable: `bind:value` follows every keystroke. */
    value?: HTMLInputAttributes["value"];
    /** Called with the new value on every input (ADR 0026's value dialect); the native `oninput` still receives the Event. */
    onvaluechange?: (value: HTMLInputAttributes["value"]) => void;
    /** The native input element, available for imperative focus or selection. */
    ref?: HTMLInputElement;
    /**
     * The control height step, matching Button's `size` (default `md`). Named
     * apart from the native `size` attribute, which is still spread through.
     */
    controlSize?: InputSize;
    /** Extra classes, merged over the canonical appearance. */
    class?: string;
  }

  let {
    ref = $bindable(),
    value = $bindable(),
    controlSize = "md",
    class: className,
    oninput,
    onvaluechange,
    ...rest
  }: Props = $props();

  // The element stays the source of truth for what was typed. `value` follows
  // it on every input and is written back only when a caller supplies one, so a
  // Field (or any spread) re-rendering around an uncontrolled Input can never
  // blank it by passing `undefined` again.
  const initial = untrack(() => value);
  // A caller's reactive spread may replace its optional ref prop. The mounted
  // control remains the source of truth for imperative updates and focus.
  let element = $state<HTMLInputElement>();
  $effect(() => { if (ref !== element) ref = element; });
  $effect(() => {
    if (element && initial !== undefined && element.type !== "file") element.defaultValue = initial === null ? "" : String(initial);
  });

  function handleInput(event: Event & { currentTarget: EventTarget & HTMLInputElement }): void {
    const element = event.currentTarget;
    value = inputValue(element);
    oninput?.(event);
    onvaluechange?.(value);
  }

  $effect(() => {
    const next = value;
    if (!element || next === undefined || element.type === "file") return;
    const text = next === null ? "" : String(next);
    if (text !== element.value) element.value = text;
  });

  // Wave 5A's accessible-name contract: inside a Field the label owns the
  // name; anywhere else a mounted control with no name warns in development.
  const insideField = getFieldContext() !== undefined;
  $effect(() => {
    if (!insideField) checkFieldName("Input", element);
  });
</script>

<input
  {...rest}
  value={initial}
  bind:this={element}
  oninput={handleInput}
  class={input({ size: controlSize, class: className })}
/>
