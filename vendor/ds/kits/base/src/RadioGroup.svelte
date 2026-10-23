<!-- One native radio group composed from the canonical Fieldset and Label. -->
<script lang="ts">
  import { untrack } from "svelte";
  import { afterFormReset } from "./core/native-controls";
  import type { HTMLFieldsetAttributes } from "svelte/elements";
  import type { Snippet } from "svelte";
  import Fieldset from "./Fieldset.svelte";
  import Label from "./Label.svelte";
  import { radioGroup, type RadioGroupOption } from "./radio-group.variants";

  interface Props extends Omit<HTMLFieldsetAttributes, "children" | "class" | "name"> {
    /** The required visible accessible name rendered as the native legend. */
    legend: string;
    /** One native form name shared by every radio in the group. */
    name: string;
    /** Caller-owned choices; the group owns their native relationship. */
    options: readonly RadioGroupOption[];
    /** Native radio rows (list, the default) or rich bordered choices (cards). */
    variant?: "list" | "cards";
    /** Noninteractive media or metadata before the option label. The label and description remain canonical. */
    optionMedia?: Snippet<[RadioGroupOption]>;
    /** The selected native value, bindable by controlled consumers. */
    value?: string;
    /** Initial uncontrolled value, restored by native form reset. */
    defaultValue?: string;
    /** The native fieldset for imperative access. */
    ref?: HTMLFieldSetElement;
    /** Called with the newly chosen value (ADR 0026's value dialect). */
    onvaluechange?: (value: string) => void;
    /** Applies native required validation to the group. */
    required?: boolean;
    /** Supporting text announced with every radio. */
    help?: string;
    /** Current validation error, announced and associated with every radio. */
    error?: string;
    /** Prefix for option ids; otherwise Svelte supplies a stable generated one. */
    id?: string;
    /** Extra classes merged onto the native fieldset. */
    class?: string;
    /** Extra classes merged onto each visible option label. */
    optionClass?: string;
    /** Extra classes merged onto each native radio. */
    controlClass?: string;
  }

  const generatedId = $props.id();
  let {
    legend,
    name,
    options,
    variant = "list",
    optionMedia,
    value = $bindable(),
    defaultValue,
    ref = $bindable(),
    onvaluechange,
    required = false,
    help,
    error,
    id,
    class: className,
    optionClass,
    controlClass,
    ...rest
  }: Props = $props();
  const initial = untrack(() => value ?? defaultValue);
  let local = $state(initial);
  const selected = $derived(value ?? local);
  let element = $state<HTMLFieldSetElement>();
  $effect(() => { if (ref !== element) ref = element; });
  $effect(() => {
    rest.form;
    return afterFormReset(element?.form, () => {
      value = initial; local = initial;
      element?.querySelectorAll<HTMLInputElement>('input[type="radio"]').forEach(input => { input.checked = input.value === initial; });
    });
  });
  const styles = $derived(radioGroup({ variant }));
  const idPrefix = $derived(id ?? `${generatedId}-option`);
</script>

<Fieldset {...rest} bind:ref={element} {legend} {help} {error} class={styles.root({ class: className })}>
  {#snippet children(control)}
    <div class={styles.list()} data-radio-list data-variant={variant}>
      {#each options as option, index (option.value)}
        {@const optionId = `${idPrefix}-${index}`}
        <Label class={styles.option({ class: optionClass })} for={optionId}>
          <span class={styles.choice()} data-choice-control>
            <input
              {...control}
              aria-labelledby={`${optionId}-label`}
              aria-describedby={[control["aria-describedby"], option.description ? `${optionId}-description` : undefined].filter(Boolean).join(" ") || undefined}
              id={optionId}
              type="radio"
              {name}
              value={option.value}
              defaultChecked={initial === option.value}
              checked={selected === option.value}
              form={rest.form}
              onchange={(event) => { local = event.currentTarget.value; value = local; onvaluechange?.(local); }}
              {required}
              disabled={option.disabled}
              class={styles.control({ class: controlClass })}
            />
            <span class={styles.box()} aria-hidden="true" data-choice-box></span>
            <span class={styles.dot()} aria-hidden="true" data-choice-mark></span>
          </span>
          <span class={styles.content()}>
            {@render optionMedia?.(option)}
            <span id={`${optionId}-label`} class={styles.label()}>{option.label}</span>
            {#if option.description}<span id={`${optionId}-description`} class={styles.description()}>{option.description}</span>{/if}
          </span>
        </Label>
      {/each}
    </div>
  {/snippet}
</Fieldset>
