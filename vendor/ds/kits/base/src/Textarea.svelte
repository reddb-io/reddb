<!--
  Textarea — the canonical Base multiline entry.

  It remains a native <textarea>, so rows, wrapping, constraints, validity,
  form submission, events and focus all stay under the platform contract.
-->
<script lang="ts">
  import { checkFieldName, getFieldContext } from "./a11y";
  import { untrack } from "svelte";
  import type { HTMLTextareaAttributes } from "svelte/elements";
  import { textarea } from "./textarea.variants";

  interface Props extends Omit<HTMLTextareaAttributes, "class" | "value"> {
    /** The current value. Bindable: `bind:value` follows every keystroke. */
    value?: HTMLTextareaAttributes["value"];
    /** Called with the new value on every input (ADR 0026's value dialect); the native `oninput` still receives the Event. */
    onvaluechange?: (value: string) => void;
    /** The native textarea, available for imperative focus or selection. */
    ref?: HTMLTextAreaElement;
    /** Extra classes, merged over the canonical appearance. */
    class?: string;
  }

  let {
    ref = $bindable(),
    value = $bindable(),
    class: className,
    oninput,
    onvaluechange,
    ...rest
  }: Props = $props();

  // As Input: the element owns what was typed; `value` follows it and is written
  // back only when a caller supplies one.
  const initial = untrack(() => value);

  function handleInput(event: Event & { currentTarget: EventTarget & HTMLTextAreaElement }): void {
    value = event.currentTarget.value;
    oninput?.(event);
    onvaluechange?.(event.currentTarget.value);
  }

  $effect(() => {
    const next = value;
    if (!ref || next === undefined || next === null) return;
    if (String(next) !== ref.value) ref.value = String(next);
  });

  // Wave 5A's accessible-name contract: inside a Field the label owns the
  // name; anywhere else a mounted control with no name warns in development.
  const insideField = getFieldContext() !== undefined;
  $effect(() => {
    if (!insideField) checkFieldName("Textarea", ref);
  });
</script>

<textarea
  {...rest}
  value={initial}
  bind:this={ref}
  oninput={handleInput}
  class={textarea({ class: className })}
></textarea>
