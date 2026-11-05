<!-- One expandable region, composed from Bits UI Collapsible and the canonical Button. -->
<script lang="ts">
  import type { DisclosureContract } from "./core/contracts/disclosure";
  import type { Snippet } from "svelte";
  import { Collapsible as Bits } from "bits-ui";
  import Button from "./Button.svelte";
  import { warnDeprecated } from "./deprecation";
  import { sanitizeCollapsibleStyle } from "./collapsible.style";
  import { disclosure } from "./disclosure.variants";

  interface Props extends DisclosureContract {
    /** Called with the next open state (ADR 0026's open dialect). */
    onopenchange?: (open: boolean) => void;
    /** @deprecated Renamed `onopenchange` (ADR 0026); removed next release. */
    onchange?: (open: boolean) => void;
    /** Content of the expandable region, rendered only while it is open. */
    children?: Snippet;
    /** Extra classes merged onto the disclosure root. */
    class?: string;
  }

  let {
    label,
    open = $bindable(false),
    disabled = false,
    onopenchange,
    onchange,
    children,
    class: className,
    triggerClass,
    contentClass,
  }: Props = $props();

  const uid = $props.id();
  const triggerId = `${uid}-trigger`;
  const styles = disclosure();

  function update(next: boolean): void {
    open = next;
    onopenchange?.(next);
    onchange?.(next);
  }

  $effect(() => {
    if (onchange) warnDeprecated("Disclosure", "onchange", "onopenchange");
  });
</script>

<Bits.Root
  {open}
  onOpenChange={update}
  {disabled}
  data-disclosure
  class={styles.root({ class: className })}
>
  <Bits.Trigger>
    {#snippet child({ props })}
      <Button
        {...props}
        id={triggerId}
        variant="secondary"
        size="sm"
        class={styles.trigger({ class: triggerClass })}
      >
        {label}
        <span aria-hidden="true" class={styles.indicator()}>⌄</span>
      </Button>
    {/snippet}
  </Bits.Trigger>
  <Bits.Content forceMount={true}>
    {#snippet child({ props, open: contentOpen })}
      {#if contentOpen}
        <div
          {...props}
          style={sanitizeCollapsibleStyle(props.style)}
          role="region"
          aria-labelledby={triggerId}
          class={styles.content({ class: contentClass })}
        >
          {@render children?.()}
        </div>
      {/if}
    {/snippet}
  </Bits.Content>
</Bits.Root>
