<script lang="ts">
  import type { Snippet } from "svelte";
  import { DescriptionList } from "@reddb-io/design-system/base";
  import type { PropertyPanelContract } from "../core/contracts/propertypanel";
  import { operational } from "../operational.variants";

  interface Props extends PropertyPanelContract {
    /** Caller-owned panel actions. */
    actions?: Snippet;
    /** Additional root classes. */
    class?: string;
  }
  let {
    title,
    items,
    description,
    actions,
    class: className,
  }: Props = $props();
  const s = operational();
</script>

<section
  data-property-panel
  aria-label={title}
  class={[s.root({ class: className }), s.body()].join(" ")}
>
  <div class={s.header()}>
    <p class={s.title()}>{title}</p>
    {#if actions}<div class={s.actions()}>{@render actions()}</div>{/if}
  </div>
  {#if description}<p class={s.description()}>
      {description}
    </p>{/if}<DescriptionList
    gap="sm"
    class={s.properties()}
    items={items.map((item) => ({
      term: item.label,
      detail: String(item.value),
    }))}
  />
</section>
