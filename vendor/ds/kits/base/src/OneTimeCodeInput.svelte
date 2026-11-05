<!-- A visibly named OTP group composed from Fieldset and the canonical Input recipe.
     Each segment remains native while paste and focus move across the code. -->
<script lang="ts">
  import { untrack } from "svelte";
  import type { HTMLFieldsetAttributes } from "svelte/elements";
  import Fieldset from "./Fieldset.svelte";
  import { oneTimeCodeInput } from "./one-time-code-input.variants";
  import { codeLength, codeCharacters, codeSegments, type OneTimeCodeInputMode } from "./core/one-time-code";
  import { afterFormReset, associatedForm } from "./core/native-controls";

  interface Props extends Omit<HTMLFieldsetAttributes, "children" | "class"> {
    /** The required visible accessible name rendered as the native legend. */
    label: string;
    /** Native form name receiving the complete code. */
    name?: string;
    /** Positive integer number of one-character segments. Defaults to 6. */
    length?: number;
    /** Complete code, bindable by controlled consumers. */
    value?: string;
    /** Initial uncontrolled code, restored by native form reset. Defaults to empty. */
    defaultValue?: string;
    /** Supporting text associated with every segment. */
    help?: string;
    /** Current validation error announced and associated with every segment. */
    error?: string;
    /** Applies native required validation to every segment. */
    required?: boolean;
    /** Input mode used by every segment. */
    inputmode?: OneTimeCodeInputMode;
    /** Native pattern used by every segment. */
    pattern?: string;
    /** Prefix for segment ids; otherwise Svelte supplies a stable generated one. */
    id?: string;
    /** Called when every segment contains a character. */
    oncomplete?: (value: string) => void;
    /** Called with the new value whenever it changes (ADR 0026's value dialect). */
    onvaluechange?: (value: string) => void;
    /** Extra classes merged onto the native fieldset. */
    class?: string;
    /** Extra classes merged onto every native segment. */
    segmentClass?: string;
  }

  const generatedId = $props.id();
  let {
    label,
    name,
    length = 6,
    value = $bindable(),
    defaultValue = "",
    help,
    error,
    required = false,
    inputmode = "numeric",
    pattern = "[0-9]*",
    id,
    oncomplete,
    onvaluechange,
    class: className,
    segmentClass,
    ...rest
  }: Props = $props();

  const idPrefix = $derived(id ?? `${generatedId}-segment`);
  const helpId = $derived(`${idPrefix}-help`);
  const errorId = $derived(`${idPrefix}-error`);
  const describedBy = $derived([help ? helpId : undefined, error ? errorId : undefined].filter(Boolean).join(" ") || undefined);
  const count = $derived(codeLength(length));
  const initial = untrack(() => (value ?? defaultValue).slice(0, count));
  const current = $derived((value ?? initial).slice(0, count));
  let segmentValues = $state(untrack(() => codeSegments(initial, count)));
  let controls = $state<HTMLInputElement[]>([]);
  const styles = oneTimeCodeInput();

  $effect(() => {
    const joined = segmentValues.join("");
    if (current !== joined || segmentValues.length !== count) {
      segmentValues = codeSegments(current, count);
    }
  });

  $effect(() => {
    return afterFormReset(associatedForm(controls[0], rest.form), () => {
      value = initial.slice(0, count); segmentValues = codeSegments(value, count);
      segmentValues.forEach((character, index) => { if (controls[index]) controls[index]!.value = character; });
    });
  });

  function publish(): void {
    const before = current;
    value = segmentValues.join("");
    if (value !== before) onvaluechange?.(value);
    if (segmentValues.every(Boolean)) oncomplete?.(value);
  }

  function focus(index: number): void {
    controls[Math.max(0, Math.min(index, count - 1))]?.focus();
  }

  function handleInput(event: Event, index: number): void {
    const input = event.currentTarget as HTMLInputElement;
    const character = codeCharacters(input.value, input.value.length).at(-1) ?? "";
    input.value = character;
    segmentValues[index] = character;
    publish();
    if (character) focus(index + 1);
  }

  function handlePaste(event: ClipboardEvent, index: number): void {
    const pasted = event.clipboardData?.getData("text") ?? "";
    const characters = codeCharacters(pasted, count - index);
    if (characters.length === 0) return;

    event.preventDefault();
    for (const [offset, character] of characters.entries()) {
      segmentValues[index + offset] = character;
    }
    publish();
    focus(index + characters.length - 1);
  }

  function handleKeydown(event: KeyboardEvent, index: number): void {
    if (event.key === "ArrowLeft") {
      event.preventDefault();
      focus(index - 1);
    } else if (event.key === "ArrowRight") {
      event.preventDefault();
      focus(index + 1);
    } else if (event.key === "Home") {
      event.preventDefault();
      focus(0);
    } else if (event.key === "End") {
      event.preventDefault();
      focus(count - 1);
    } else if (event.key === "Backspace" && !segmentValues[index]) {
      focus(index - 1);
    }
  }
</script>

<Fieldset
  {...rest}
  legend={label}
  class={styles.root({ class: className })}
  data-one-time-code-input
  aria-invalid={error ? "true" : undefined}
>
  <div class={styles.list()} data-otp-list>
    {#each segmentValues as character, index}
      <input
        bind:this={controls[index]}
        id={`${idPrefix}-${index}`}
        type="text"
        value={character}
        maxlength={1}
        {inputmode}
        {pattern}
        autocomplete={index === 0 ? "one-time-code" : "off"}
        {required}
        form={rest.form}
        aria-label={`Digit ${index + 1} of ${count}`}
        aria-describedby={describedBy}
        aria-invalid={error ? "true" : undefined}
        data-otp-segment
        class={styles.segment({ class: segmentClass })}
        oninput={(event) => handleInput(event, index)}
        onpaste={(event) => handlePaste(event, index)}
        onkeydown={(event) => handleKeydown(event, index)}
      />
    {/each}
  </div>

  {#if name}<input type="hidden" {name} value={current} form={rest.form} />{/if}
  {#if help}<p class={styles.help()} id={helpId}>{help}</p>{/if}
  {#if error}<p class={styles.error()} id={errorId} role="alert">{error}</p>{/if}
</Fieldset>
