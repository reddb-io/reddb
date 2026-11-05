<!-- Bits UI keyboard and expanded-state behavior with canonical Button triggers. -->
<script lang="ts">
  import type { AccordionContract } from "./core/contracts/accordion";
  import { untrack, type Snippet } from "svelte";
  import { Accordion as Bits } from "bits-ui";
  import Button from "./Button.svelte";
  import { warnDeprecated } from "./deprecation";
  import { sanitizeCollapsibleStyle } from "./collapsible.style";
  import { accordion, type AccordionItem } from "./accordion.variants";

  interface Props extends AccordionContract {
    /** @deprecated Renamed `value` (`bind:value`, ADR 0026); removed next release. */
    expanded?: string[];
    /** Called with the expanded item values after each change. */
    onvaluechange?: (value: readonly string[]) => void;
    /** @deprecated Renamed `onvaluechange` (ADR 0026); removed next release. */
    onchange?: (value: readonly string[]) => void;
    /**
     * Panel content for a section, called with `{ value }` of the item it belongs to so the caller
     * picks the content.
     */
    children?: Snippet<[{ value: string }]>;
    /**
     * Extra classes merged onto the accordion root, the element that carries the accessible name.
     */
    class?: string;
  }

  let {
    label,
    items,
    value = $bindable(),
    expanded = $bindable(),
    multiple = false,
    disabled = false,
    onvaluechange,
    onchange,
    children,
    class: className,
    triggerClass,
    contentClass,
  }: Props = $props();

  const uid = $props.id();
  const styles = accordion();

  // A caller that still binds the deprecated `expanded` (and not `value`) keeps
  // driving the Accordion through it for one release; `value` wins otherwise.
  const aliased = untrack(() => value === undefined && expanded !== undefined);
  const current = $derived((aliased ? expanded : value) ?? []);

  function update(next: string[]): void {
    const normalized = multiple ? next : next.slice(-1);
    if (aliased) expanded = normalized;
    else value = normalized;
    onvaluechange?.(normalized);
    onchange?.(normalized);
  }

  $effect(() => {
    if (aliased) warnDeprecated("Accordion", "expanded", "value (bind:value)");
    if (onchange) warnDeprecated("Accordion", "onchange", "onvaluechange");
  });
</script>

<Bits.Root
  type="multiple"
  value={current}
  onValueChange={update}
  {disabled}
  loop={true}
  data-accordion
  aria-label={label}
  class={styles.root({ class: className })}
>
  {#each items as item, index (item.value)}
    {@const triggerId = `${uid}-${index}-trigger`}
    {@const contentId = `${uid}-${index}-content`}
    <Bits.Item value={item.value} disabled={item.disabled} class={styles.item()}>
      <Bits.Header class={styles.header()}>
        <Bits.Trigger>
          {#snippet child({ props })}
            <Button
              {...props}
              id={triggerId}
              aria-controls={contentId}
              variant="ghost"
              size="sm"
              class={styles.trigger({ class: triggerClass })}
            >
              <span class={styles.label()} data-accordion-label>{item.label}</span>
              <span aria-hidden="true" class={styles.indicator()}>⌄</span>
            </Button>
          {/snippet}
        </Bits.Trigger>
      </Bits.Header>
      <Bits.Content>
        {#snippet child({ props })}
          <div
            {...props}
            style={sanitizeCollapsibleStyle(props.style)}
            id={contentId}
            role="region"
            aria-labelledby={triggerId}
            class={styles.content({ class: contentClass })}
          >
            {@render children?.({ value: item.value })}
          </div>
        {/snippet}
      </Bits.Content>
    </Bits.Item>
  {/each}
</Bits.Root>
