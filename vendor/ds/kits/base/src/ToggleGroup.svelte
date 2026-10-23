<!-- Exactly one pressed option, composed from canonical Fieldset and ToggleButton. -->
<script lang="ts">
  import { untrack } from "svelte";
  import { afterFormReset, associatedForm } from "./core/native-controls";
  import { toggleGroupTarget, toggleGroupValue, type ToggleGroupContract } from "./core/toggle-group";
  import type { HTMLFieldsetAttributes } from "svelte/elements";
  import Fieldset from "./Fieldset.svelte";
  import ToggleButton from "./ToggleButton.svelte";
  import { toggleGroup, type ToggleGroupOption } from "./toggle-group.variants";

  interface Props extends ToggleGroupContract, Omit<HTMLFieldsetAttributes, "children" | "class" | "name" | "disabled"> {
    /** Extra classes merged onto the native fieldset. */
    class?: string;
    /** Called with a newly selected option; reset does not emit a user change. */
    onvaluechange?: (value: string) => void;
  }

  let {
    legend,
    options,
    value = $bindable(),
    defaultValue,
    name,
    disabled = false,
    help,
    error,
    class: className,
    listClass,
    optionClass,
    onvaluechange,
    ...rest
  }: Props = $props();
  const styles = $derived(toggleGroup({ invalid: Boolean(error) }));
  const initial = untrack(() => value ?? defaultValue);
  let local = $state(initial);
  const selected = $derived(toggleGroupValue(options, value ?? local));
  let element = $state<HTMLFieldSetElement>();
  let list: HTMLDivElement;
  $effect(() => afterFormReset(associatedForm(element, rest.form), () => { local = initial; value = initial; }));

  function choose(option: ToggleGroupOption): void {
    if (disabled || element?.matches(":disabled") || option.disabled || option.value === selected) return;
    value = option.value;
    onvaluechange?.(option.value);
  }

  function move(event: KeyboardEvent, current: number): void {
    if (disabled || element?.matches(":disabled")) return;
    const next = toggleGroupTarget(options, current, event.key, getComputedStyle(list).direction === "rtl");
    if (next === undefined) return;
    event.preventDefault();
    choose(options[next]!);
    list.querySelectorAll<HTMLButtonElement>("button")[next]?.focus();
  }
</script>

<Fieldset {...rest} bind:ref={element} {legend} {disabled} {help} {error} class={styles.root({ class: className })} data-toggle-group>
  {#snippet children(control)}
    <div bind:this={list} class={styles.list({ class: listClass })} role="group" aria-label={legend}>
      {#each options as option, index (option.value)}
        <ToggleButton
          {...control}
          label={option.label}
          pressed={option.value === selected}
          allowUnpress={false}
          disabled={disabled || option.disabled}
          class={styles.option({ class: optionClass })}
          tabindex={option.value === selected ? 0 : -1}
          data-toggle-value={option.value}
          onclick={() => choose(option)}
          onkeydown={(event) => move(event, index)}
        />
      {/each}
    </div>
    {#if name}
      <input type="hidden" {name} value={selected} {disabled} form={rest.form} />
    {/if}
  {/snippet}
</Fieldset>
