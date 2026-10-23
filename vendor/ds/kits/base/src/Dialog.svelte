<!--
  A modal Dialog on the platform's top-layer <dialog> (ADR 0017).

  `showModal()` already renders above every stacking context and makes the rest
  of the document inert, so the DS owns only what the platform leaves open: a
  controllable `open`, labelling, focus entry and return, light dismiss, and
  scroll containment. The trigger is optional — omit `triggerLabel` and drive
  `bind:open` from a menu item, a command or a route instead.
-->


<script lang="ts">
  import { createModalController } from "./core/modal";
  import type { DialogContract } from "./core/contracts/dialog";
  import { untrack, type Snippet } from "svelte";
  import type { HTMLDialogAttributes } from "svelte/elements";
  import { button } from "./button.variants";
  import { dialog as dialogAppearance, dialogClose, type DialogSize } from "./dialog.variants";

  interface Props extends DialogContract, Omit<HTMLDialogAttributes, keyof DialogContract | "aria-describedby" | "aria-labelledby" | "class"> {
    /** Replaces the visible title and default close control with a caller-owned header. The required title remains the accessible name; the header receives the canonical close operation. A custom header keeps the body in its own scrolling region. */
    header?: Snippet<[{ close: () => void }]>;
    /**
     * Optional custom trigger content; `triggerLabel` remains its accessible name. Words it shows
     * must appear, in order, in `triggerLabel`, ideally first (WCAG 2.5.3, Label in Name).
     */
    trigger?: Snippet;
    /** Dialog body. */
    children?: Snippet;
    /** Optional action region with the canonical close operation. */
    actions?: Snippet<[{ close: () => void }]>;
    /** Called whenever the Dialog opens or closes, by any path. */
    onopenchange?: (open: boolean) => void;
    /** Extra classes merged onto the modal surface. */
    class?: string;
  }

  let {
    triggerLabel,
    title,
    description,
    header,
    trigger,
    children,
    actions,
    initialFocus = "body",
    showClose = true,
    open = $bindable(false),
    onopenchange,
    dismissible = true,
    size = "md",
    class: className,
    ...rest
  }: Props = $props();

  const uid = $props.id();
  const titleId = `${uid}-title`;
  const descriptionId = `${uid}-description`;

  let dialog: HTMLDialogElement | undefined = $state();
  let controller = $state<ReturnType<typeof createModalController>>();
  $effect(() => {
    if (!dialog) return;
    const next = createModalController(dialog, { initialFocus: () => initialFocus, dismissible: () => dismissible, requestOpen: setOpen });
    controller = next;
    return () => next.destroy();
  });
  function apply(next: boolean) { controller?.sync(next); }

  function setOpen(next: boolean): void {
    const changed = open !== next;
    open = next;
    apply(next);
    if (changed) onopenchange?.(next);
  }

  function close(): void {
    setOpen(false);
  }

  // A caller that drives `bind:open` from outside reaches the element here;
  // the DS's own trigger, close control and keys apply it synchronously above.
  $effect(() => {
    const wanted = open;
    const lifecycle = controller;
    untrack(() => lifecycle?.sync(wanted));
  });

</script>

{#if triggerLabel}
  <button
    type="button"
    data-dialog-trigger
    aria-label={triggerLabel}
    aria-haspopup="dialog"
    aria-expanded={open}
    class={button({ variant: "secondary", size: "sm" })}
    onclick={() => setOpen(true)}
  >
    {#if trigger}{@render trigger()}{:else}{triggerLabel}{/if}
  </button>
{/if}

<dialog
  bind:this={dialog}
  {...(rest as Record<string, unknown>)}
  aria-modal="true"
  aria-labelledby={titleId}
  aria-describedby={description ? descriptionId : undefined}
  class={dialogAppearance({ size, class: [header ? "open:flex open:flex-col overflow-hidden" : "", className].filter(Boolean).join(" ") })}
  tabindex="-1"
>
  <h2
    id={titleId}
    data-dialog-title
    class={[
      header ? "sr-only" : "text-heading",
      showClose && "pr-[var(--reddb-spatial-control-height-md)]",
    ]}
  >{title}</h2>
  {#if header}<div data-dialog-header class="shrink-0">{@render header({ close })}</div>{/if}
  {#if description}
    <p id={descriptionId} data-dialog-description class="mt-[var(--reddb-spatial-gap-sm)] text-ink-muted">
      {description}
    </p>
  {/if}
  {#if showClose && !header}
    <button
      type="button"
      data-dialog-close
      aria-label={`Close ${title}`}
      class={button({ variant: "ghost", size: "sm", class: dialogClose() })}
      onclick={close}><span aria-hidden="true">×</span></button
    >
  {/if}
  {#if children}
    <div data-dialog-body class={header ? "min-h-0 flex-1 overflow-y-auto" : "mt-[var(--reddb-spatial-gap-lg)]"}>{@render children()}</div>
  {/if}
  {#if actions}
    <div data-dialog-actions>{@render actions({ close })}</div>
  {/if}
</dialog>
