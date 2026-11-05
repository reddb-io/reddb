<!-- One announced active filter, composed from the canonical Base ToggleGroup. -->
<script lang="ts">
  import type { FilterContract } from "../core/contracts/filter";
  import { ToggleGroup } from "@reddb-io/design-system/base";
  import type { HTMLFieldsetAttributes } from "svelte/elements";
  import { filter, type FilterOption } from "./filter.variants";

  interface Props extends FilterContract, Omit<HTMLFieldsetAttributes, keyof FilterContract | "children" | "class" | "name"> {
    /** Extra classes, merged over the root slot's own, onto the ToggleGroup's fieldset. */
    class?: string;
    /** Called with the chosen option's value whenever the active filter changes. */
    onvaluechange?: (value: string) => void;
  }

  let {
    legend,
    options,
    value = $bindable(""),
    name,
    disabled = false,
    class: className,
    listClass,
    optionClass,
    onvaluechange,
    ...rest
  }: Props = $props();
  const styles = filter();
</script>

<ToggleGroup
  {...rest}
  {legend}
  {options}
  bind:value
  {name}
  {disabled}
  data-filter
  class={styles.root({ class: className })}
  listClass={styles.list({ class: listClass })}
  {optionClass}
  {onvaluechange}
/>
