<script module lang="ts">
  export type { WorkspaceTab } from "../core/workspace-tabs";
</script>

<script lang="ts">
  import { type Snippet } from "svelte";
  import { Tabs as Bits } from "bits-ui";
  import { Button, quietControl } from "@reddb-io/design-system/base";
  import {
    workspaceTabs,
    workspaceSelection,
    mountWorkspaceFocus,
  } from "../core/workspace-tabs";
  import { workspaceTabsAppearance } from "../workspace-tabs.variants";
  import type { WorkspaceTabsContract } from "../core/contracts/workspacetabs";
  interface Props extends WorkspaceTabsContract {
    /** Native content per document; inactive panels remain mounted and hidden. */
    children?: Snippet<[{ id: string }]>;
    /** Additional composition classes. */
    class?: string;
  }
  let {
    label,
    items,
    selected,
    onselectedchange,
    oncloserequest,
    closeLabel = "Close",
    dirtyLabel = "Unsaved changes",
    busyLabel = "Running",
    emptyLabel = "No open documents",
    children,
    class: className,
  }: Props = $props();
  const s = workspaceTabsAppearance(),
    uid = $props.id();
  let local = $state<string | null>(null);
  let root: HTMLDivElement;
  let focus: ReturnType<typeof mountWorkspaceFocus> | undefined;
  const documents = $derived(workspaceTabs(items));
  const active = $derived(
    workspaceSelection(documents, selected === undefined ? local : selected),
  );
  function select(id: string) {
    if (id === active) return;
    if (selected === undefined) local = id;
    onselectedchange?.(id);
  }
  function requestClose(id: string) {
    const item = documents.find((item) => item.id === id);
    if (item && !item.disabled && item.closable !== false) oncloserequest?.(id);
  }
  $effect(() => {
    focus = mountWorkspaceFocus(root);
    return () => focus?.destroy();
  });
  $effect(() => {
    focus?.sync(documents, active);
  });
</script>

<div bind:this={root} data-workspace-tabs class={s.root({ class: className })}>
  <Bits.Root
    value={active}
    onValueChange={select}
    activationMode="automatic"
    loop
  >
    <div
      data-workspace-scroll
      class={s.frame()}
      style:grid-template-columns={`repeat(${Math.max(documents.length, 1)},max-content)`}
    >
      <Bits.List
        aria-label={label}
        class={s.list()}
        onkeydown={(event) => {
          if (
            event.key !== "Delete" ||
            !(event.target instanceof HTMLElement) ||
            event.target.getAttribute("role") !== "tab"
          )
            return;
          const id = event.target.dataset.workspaceTab;
          if (id && oncloserequest) {
            event.preventDefault();
            requestClose(id);
          }
        }}
      >
        {#each documents as item (item.id)}
          <Bits.Trigger
            value={item.id}
            disabled={item.disabled}
            data-workspace-item={item.id}
            data-workspace-tab={item.id}
            aria-label={`${item.label}${item.busy ? `, ${busyLabel}` : ""}`}
            aria-describedby={item.dirty || item.busy
              ? `${uid}-${encodeURIComponent(item.id)}-status`
              : undefined}
            class={quietControl({
              ink: "foreground",
              class: s.trigger({
                withClose: !!oncloserequest && item.closable !== false,
              }),
            })}
          >
            <span>{item.label}</span>
            {#if item.dirty || item.busy}<span
                class={s.status()}
                aria-hidden="true"
                >{item.dirty ? "•" : ""}{item.busy ? ` ${busyLabel}` : ""}</span
              ><span
                id={`${uid}-${encodeURIComponent(item.id)}-status`}
                class="sr-only"
                >{[item.dirty ? dirtyLabel : "", item.busy ? busyLabel : ""]
                  .filter(Boolean)
                  .join(", ")}</span
              >{/if}
          </Bits.Trigger>
        {/each}
      </Bits.List>
      {#each documents as item, index (item.id)}
        {#if oncloserequest && item.closable !== false}<div
            data-workspace-item={item.id}
            class={s.close()}
            style:grid-column={index + 1}
          >
            <Button
              size="sm"
              variant="ghost"
              disabled={item.disabled}
              tabindex={active === item.id ? 0 : -1}
              class={s.closeButton()}
              aria-label={`${closeLabel} ${item.label}`}
              onclick={() => requestClose(item.id)}
              ><span aria-hidden="true">×</span></Button
            >
          </div>{/if}
      {/each}
    </div>
    {#each documents as item (item.id)}<Bits.Content
        value={item.id}
        data-workspace-panel={item.id}
        class={s.panel()}
        tabindex={0}>{@render children?.({ id: item.id })}</Bits.Content
      >{/each}
  </Bits.Root>
  {#if !documents.length}<p class={s.empty()}>{emptyLabel}</p>{/if}
</div>
