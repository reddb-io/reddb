<!-- One native radio value composed from the canonical Fieldset and Label. -->
<script lang="ts">
  import { untrack } from "svelte";
  import { afterFormReset, ratingStops } from "./core/native-controls";
  import type { HTMLFieldsetAttributes } from "svelte/elements";
  import Fieldset from "./Fieldset.svelte";
  import Label from "./Label.svelte";
  import { rating } from "./rating.variants";

  interface Props extends Omit<HTMLFieldsetAttributes, "children" | "class" | "name"> {
    /** Required visible group name rendered as a native legend. */
    legend: string;
    /** One native name shared by every rating radio. */
    name: string;
    /** Current rating; zero means no option is selected. */
    value?: number;
    /** Initial uncontrolled rating, restored by native form reset. */
    defaultValue?: number;
    /** The native fieldset for imperative focus or inspection. */
    ref?: HTMLFieldSetElement;
    /** Called with the newly chosen value (ADR 0026's value dialect). */
    onvaluechange?: (value: number) => void;
    /** Number of stops, floored to at least one; non-finite values use five. */
    max?: number;
    /**
     * Marks every rating radio as required, so the native form refuses a submit while none is
     * chosen.
     */
    required?: boolean;
    /** Supporting text announced with every rating stop. */
    help?: string;
    /** Current validation error, announced and associated with every rating stop. */
    error?: string;
    /**
     * Prefix for the ids of the rating radios, joined with each stop's number; generated when
     * omitted.
     */
    id?: string;
    /** Extra classes, merged onto the fieldset root. */
    class?: string;
    /** Extra classes merged onto each stop's label, over its own. */
    optionClass?: string;
    /** Extra classes merged onto each visually hidden radio input. */
    controlClass?: string;
    /** Extra classes merged onto each star mark, over its own. */
    markClass?: string;
    /** Extra classes merged onto the live "n of max" readout under the stops. */
    outputClass?: string;
  }

  const generatedId = $props.id();
  let {
    legend,
    name,
    value = $bindable(),
    defaultValue = 0,
    ref = $bindable(),
    onvaluechange,
    max = 5,
    required = false,
    help,
    error,
    id,
    class: className,
    optionClass,
    controlClass,
    markClass,
    outputClass,
    ...rest
  }: Props = $props();
  const styles = rating();
  let element = $state<HTMLFieldSetElement>();
  let readout = $state<HTMLOutputElement>();
  const initial = untrack(() => value ?? defaultValue);
  let local = $state(initial);
  const selected = $derived(value ?? local);
  const stops = $derived(ratingStops(max));
  const idPrefix = $derived(id ?? `${generatedId}-rating`);
  $effect(() => { if (ref !== element) ref = element; });
  // Native output reset replaces its text nodes. Keep its live DOM value in sync.
  $effect(() => { if (readout) readout.value = `${selected} of ${stops.length}`; });

  $effect(() => {
    // Changing the native form target must move the reset listener too.
    rest.form;
    return afterFormReset(element?.form, () => {
      local = initial;
      value = initial;
      element?.querySelectorAll<HTMLInputElement>('input[type="radio"]').forEach(input => {
        input.checked = Number(input.value) === initial;
      });
    });
  });
</script>

<Fieldset {...rest} bind:ref={element} {legend} {help} {error} class={styles.root({ class: className })} data-rating>
  {#snippet children(control)}
    <div class={styles.list()} data-rating-list>
      {#each stops as stop}
        {@const optionId = `${idPrefix}-${stop}`}
        <Label class={styles.option({ class: optionClass })} for={optionId}>
          <input
            {...control}
            id={optionId}
            type="radio"
            {name}
            value={stop}
            defaultChecked={initial === stop}
            checked={selected === stop}
            form={rest.form}
            onchange={(event) => { local = Number(event.currentTarget.value); value = local; onvaluechange?.(value); }}
            {required}
            class={styles.control({ class: controlClass })}
          />
          <span class={styles.mark({ class: markClass })} data-rating-mark aria-hidden="true">
            {stop <= selected ? "★" : "☆"}
          </span>
          <span class="sr-only">{stop} of {stops.length}</span>
        </Label>
      {/each}
    </div>
    <output bind:this={readout} class={styles.output({ class: outputClass })} aria-live="polite">
      {untrack(() => `${selected} of ${stops.length}`)}
    </output>
  {/snippet}
</Fieldset>
