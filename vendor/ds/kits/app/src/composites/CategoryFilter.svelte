<!-- Multiple announced category filters, composed from canonical Base form contracts. -->
<script lang="ts">
  import type { CategoryFilterContract } from "../core/contracts/categoryfilter";
  import { untrack } from "svelte";
  import { Checkbox, Fieldset, afterFormReset, associatedForm } from "@reddb-io/design-system/base";
  import { warnDeprecated } from "@reddb-io/design-system/base";
  import type { HTMLFieldsetAttributes } from "svelte/elements";
  import { categoryValues } from "../core/category-filter";
  import { categoryFilter, type CategoryFilterOption } from "./category-filter.variants";

  interface Props extends CategoryFilterContract, Omit<HTMLFieldsetAttributes, keyof CategoryFilterContract | "children" | "class" | "name"> {
    /** @deprecated Renamed `value` (`bind:value`, ADR 0026); removed next release. */
    values?: string[];
    /** Extra classes, merged over the root slot's own, onto the fieldset. */
    class?: string;
    /** Called with the active category values after each change. */
    onvaluechange?: (value: string[]) => void;
    /** @deprecated Renamed `onvaluechange` (ADR 0026); removed next release. */
    onvalueschange?: (values: string[]) => void;
  }

  let {
    legend,
    options,
    value = $bindable(),
    values = $bindable(),
    name,
    disabled = false,
    class: className,
    listClass,
    optionClass,
    onvaluechange,
    onvalueschange,
    ...rest
  }: Props = $props();
  const styles = categoryFilter();

  // A caller that still binds the deprecated `values` (and not `value`) keeps
  // driving the filter through it for one release; `value` wins otherwise.
  const aliased = untrack(() => value === undefined && values !== undefined);
  const active = $derived((aliased ? values : value) ?? []);
  let element = $state<HTMLFieldSetElement>();
  const initial = untrack(() => [...active]);
  $effect(() => afterFormReset(associatedForm(element, rest.form), () => {
    if (aliased) values = [...initial]; else value = [...initial];
  }));

  $effect(() => {
    if (aliased) warnDeprecated("CategoryFilter", "values", "value (bind:value)");
    if (onvalueschange) warnDeprecated("CategoryFilter", "onvalueschange", "onvaluechange");
  });

  function change(option: string, checked: boolean): void {
    const next = categoryValues(active, option, checked);
    if (aliased) values = next;
    else value = next;
    onvaluechange?.([...next]);
    onvalueschange?.([...next]);
  }
</script>

<Fieldset
  bind:ref={element}
  {...rest}
  {legend}
  {disabled}
  data-category-filter
  class={styles.root({ class: className })}
>
  <div data-category-filter-list class={styles.list({ class: listClass })}>
    {#each options as option (option.value)}
      <Checkbox
        label={option.label}
        {name}
        value={option.value}
        checked={active.includes(option.value)}
        disabled={disabled || option.disabled}
        fieldClass={styles.option({ class: optionClass })}
        oncheckedchange={(checked) => change(option.value, checked)}
      />
    {/each}
  </div>
</Fieldset>
