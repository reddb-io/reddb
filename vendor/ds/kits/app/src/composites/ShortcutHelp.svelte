<script module lang="ts">
  export type { ShortcutGroup, ShortcutCommand } from "../core/shortcut-help";
</script>

<script lang="ts">
  import { Dialog, Kbd } from "@reddb-io/design-system/base";
  import { shortcutGroups } from "../core/shortcut-help";
  import { shortcutHelp } from "../shortcut-help.variants";
  import type { ShortcutHelpContract } from "../core/contracts/shortcuthelp";
  interface Props extends ShortcutHelpContract {
    /** Additional classes on the reference list. */
    class?: string;
  }
  let {
    title,
    groups,
    triggerLabel,
    open = $bindable(false),
    onopenchange,
    emptyLabel = "No shortcuts available",
    class: className,
  }: Props = $props();
  const s = shortcutHelp();
  const visible = $derived(shortcutGroups(groups));
</script>

<Dialog {title} {triggerLabel} bind:open {onopenchange}>
  <div data-shortcut-help class={s.root({ class: className })}>
    {#each visible as group (group.id)}<section
        class={s.group()}
        aria-label={group.label}
      >
        <h3 class={s.title()}>{group.label}</h3>
        {#if group.description}<p class={s.description()}>
            {group.description}
          </p>{/if}
        <ul class={s.list()}>
          {#each group.items as item (item.id)}<li class={s.item()}>
              <span class={s.name()}>{item.label}</span><span class={s.keys()}
                >{#each item.keys as key, index (index)}<Kbd
                    keys={[key]}
                    size="sm"
                  />{/each}</span
              >
            </li>{/each}
        </ul>
      </section>{/each}
    {#if !visible.length}<p class={s.description()}>{emptyLabel}</p>{/if}
  </div>
</Dialog>
