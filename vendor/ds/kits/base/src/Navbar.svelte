<script module lang="ts">
  import { arrangements } from "./core/navbar";
  import type { NavbarLink, NavbarCollapse, NavbarAlign } from "./core/navbar";
  export { arrangements, NAVBAR_ALIGNMENTS, NAVBAR_COLLAPSE, type NavbarLink, type NavbarCollapse, type NavbarAlign, type NavbarArrangements } from "./core/navbar";
</script>

<script lang="ts">
  import type { NavbarContract } from "./core/contracts/navbar";
  import { onDestroy, untrack, type Snippet } from "svelte";
  import { navigationDisclosure } from "./navigation";
  import Button from "./Button.svelte";
  import Icon from "./Icon.svelte";
  import Link from "./Link.svelte";
  import { navbar } from "./navbar.variants";
  import { quietControl } from "./quiet-control.variants";

  interface Props extends NavbarContract {
    /** Called with the next open state (ADR 0026's open dialect). */
    onopenchange?: (open: boolean) => void;
    /** Leading identity slot, rendered in both arrangements: typically a Logo or product name. */
    brand?: Snippet;
    /**
     * Trailing slot for controls such as a search field or account menu, rendered in the rail and
     * in the compact panel.
     */
    actions?: Snippet;
    /** Extra classes, merged over the root slot's own. */
    class?: string;
  }

  let {
    links = [],
    collapse = "responsive",
    align = "start",
    open = $bindable(false),
    onopenchange,
    label = "Main",
    menuLabel = "Menu",
    brand,
    actions,
    class: className,
  }: Props = $props();

  const uid = $props.id();
  const panelId = `${uid}-menu`;
  let root: HTMLElement | undefined = $state();
  const styles = $derived(navbar({ collapse, align, open }));
  const shown = $derived(arrangements(collapse));

  const disclosure = navigationDisclosure(() => setOpen(false));
  onDestroy(() => disclosure?.destroy());
  $effect(() => { if (open) untrack(() => disclosure?.open()); });

  function currentValue(link: NavbarLink): NavbarLink["current"] | undefined {
    if (link.current === true || link.active) return "page";
    return link.current || undefined;
  }

  function setOpen(next: boolean): void {
    if (open === next) return;
    open = next;
    onopenchange?.(next);
  }

  function dismiss(): void {
    if (!open) return;
    setOpen(false);
    root?.querySelector<HTMLButtonElement>("[data-navbar-toggle]")?.focus();
  }

  $effect(() => {
    const element = root;
    if (!open || element === undefined) return;
    const onkeydown = (event: KeyboardEvent): void => {
      if (event.key === "Escape") dismiss();
    };
    element.addEventListener("keydown", onkeydown);
    return () => element.removeEventListener("keydown", onkeydown);
  });
</script>

<nav
  bind:this={root}
  aria-label={label}
  data-navbar
  data-navbar-align={align}
  data-navbar-collapse={collapse}
  class={styles.root({ class: className })}
>
  {#if shown.rail}
    <div data-navbar-arrangement="rail" class={styles.rail()}>
      <div data-navbar-region="brand" class={styles.brand()}>{@render brand?.()}</div>
      {#if links.length > 0}
        <ul data-navbar-region="links" class={styles.links()}>
          {#each links as link (link.id)}
            <li class={styles.item()}>
              {#if link.disabled && link.href !== undefined}
                <a aria-disabled="true" data-navbar-link={link.id} class={quietControl({ class: styles.link() })}>{link.label}</a>
              {:else if link.href !== undefined}
                <Link
                  href={link.href}
                  aria-current={currentValue(link)}
                  data-navbar-link={link.id}
                  class={quietControl({ class: styles.link() })}
                  onclick={link.onselect}
                >{link.label}</Link>
              {:else}
                <Button
                  variant="ghost"
                  disabled={link.disabled}
                  aria-current={currentValue(link)}
                  data-navbar-link={link.id}
                  class={styles.link()}
                  onclick={link.onselect}
                >{link.label}</Button>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
      <div data-navbar-region="actions" class={styles.actions()}>{@render actions?.()}</div>
    </div>
  {/if}

  {#if shown.compact}
    <div data-navbar-arrangement="compact" class={styles.compact()}>
      <div data-navbar-region="brand" class={styles.brand()}>{@render brand?.()}</div>
      <Button
        variant="ghost"
        class={styles.toggle()}
        data-navbar-toggle
        aria-label={menuLabel}
        aria-expanded={open}
        aria-controls={panelId}
        onclick={() => setOpen(!open)}
      ><Icon icon="menu" size="sm" aria-hidden="true" />{menuLabel}</Button>
    </div>
    <div id={panelId} data-navbar-panel hidden={!open} class={styles.panel()}>
      {#if links.length > 0}
        <ul data-navbar-region="links" class={styles.panelLinks()}>
          {#each links as link (link.id)}
            <li>
              {#if link.disabled && link.href !== undefined}
                <a aria-disabled="true" data-navbar-link={link.id} class={quietControl({ class: styles.panelLink() })}>{link.label}</a>
              {:else if link.href !== undefined}
                <Link
                  href={link.href}
                  aria-current={currentValue(link)}
                  data-navbar-link={link.id}
                  class={quietControl({ class: styles.panelLink() })}
                  onclick={() => {
                    link.onselect?.();
                    setOpen(false);
                  }}
                >{link.label}</Link>
              {:else}
                <Button
                  variant="ghost"
                  disabled={link.disabled}
                  aria-current={currentValue(link)}
                  data-navbar-link={link.id}
                  class={styles.panelLink()}
                  onclick={() => {
                    link.onselect?.();
                    setOpen(false);
                  }}
                >{link.label}</Button>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
      <div data-navbar-region="actions" class={styles.actions()}>{@render actions?.()}</div>
    </div>
  {/if}
</nav>
