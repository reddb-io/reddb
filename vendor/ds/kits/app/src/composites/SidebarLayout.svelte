<!--
  The application-specific sidebar arrangement: one named complementary
  landmark and one main region. The region is the page's `<main>` only when no
  ApplicationShell above it already owns that landmark (DESIGN.md,
  "Landmarks"); `main` overrides the default either way. Both contain canonical Base Stacks whose
  rhythm defaults to the Density layout tier (ADR 0024); caller content stays
  the caller's. Without a rail it steps on its own width (ADR 0021); with a
  rail it is the page's chrome and steps on the viewport (see the variants).
-->
<script lang="ts">
  import type { SidebarLayoutContract } from "../core/contracts/sidebarlayout";
  import { onDestroy, tick, untrack, type Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { Badge, Button, Drawer, Icon, Stack, StatusIndicator, navigationDisclosure, provideNavigationDisclosures, type StackGap } from "@reddb-io/design-system/base";
  import { claimMainLandmark, mainLandmarkOwned } from "./main-landmark";
  import { responsiveNavigationLayout, sidebarLayout, type SidebarSide } from "./sidebar-layout.variants";

  import SidebarRail from "./SidebarRail.svelte";
  import BottomNavigation, { type BottomNavigationItem } from "./BottomNavigation.svelte";
  import NavItem from "../primitives/NavItem.svelte";
  import { navigationBadge, navigationItems, navigationMarkerLabel, type PrimaryNavigationItem } from "./navigation.behavior";

  interface Props extends SidebarLayoutContract, Omit<HTMLAttributes<HTMLDivElement>, "class"> {
    /** Sidebar navigation, filters or other content. In primary navigation mode receives the open section and the close operation; zero-argument snippets remain valid. */
    sidebar?: Snippet<[{ item: PrimaryNavigationItem | undefined; close: () => void }]>;
    /** Global Navbar or other product-wide header. Its disclosures share a scope with contextual menus, so opening one closes the other. */
    header?: Snippet;
    /** Optional navigation rail paired with the existing sidebar panel. The track follows its rendered width, including visible rail labels. */
    rail?: Snippet;
    /** Notifies controlled consumers when the built-in drawer dismissal closes the panel. */
    onpanelopenchange?: (open: boolean) => void;
    /** Primary caller-owned page content. */
    children?: Snippet;
    /** Extra classes merged onto the layout's root. */
    class?: string;
  }

  let {
    side = "start",
    sidebarLabel = "Sidebar",
    mainId,
    main,
    sidebar,
    navigation,
    header,
    dockable = true,
    currentId,
    navigationLabel = "Main navigation",
    showRailLabels = false,
    moreLabel = "More",
    rail,
    railOpen = $bindable(true),
    panelOpen = $bindable(false),
    onpanelopenchange,
    children,
    gap = "layout-md",
    class: className,
    ...rest
  }: Props = $props();

  // ApplicationShell owns the page's one `<main>` (DESIGN.md, "Landmarks").
  // Context is read once, at setup, as Svelte requires.
  const ownedAbove = mainLandmarkOwned();
  const rendersMain = $derived(main ?? !ownedAbove);
  const regionId = $derived(rendersMain ? (mainId ?? "main-content") : mainId);
  if (untrack(() => main) ?? !ownedAbove) claimMainLandmark();

  let railElement = $state<HTMLElement>();
  let railWidth = $state(0);
  $effect(() => {
    if (!railElement || !rail) return;
    const element = railElement;
    const update = () => (railWidth = element.getBoundingClientRect().width);
    update();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(update);
    observer.observe(element);
    return () => observer.disconnect();
  });
  const hasRail = $derived(rail !== undefined);
  const slots = $derived(sidebarLayout({ side, rail: hasRail, railOpen, panelOpen }));

  provideNavigationDisclosures();
  const disclosure = navigationDisclosure(() => { if(navigation && compact) setPanelOpen(false); });
  onDestroy(() => disclosure?.destroy());
  $effect(() => { if (navigation && compact && panelOpen) untrack(() => disclosure?.open()); });

  const uid = $props.id();
  const menuId = `${uid}-navigation-menu`;
  const moreId = `${uid}-more`;
  const primaryItems = $derived.by(() => {
    if (!navigation) return [];
    if (rail) throw new Error("SidebarLayout: choose navigation items or a rail snippet, not both");
    if (new Set(navigation.map(item => item.id)).size !== navigation.length) throw new Error("SidebarLayout navigation ids must be unique");
    if (!currentId) throw new Error("SidebarLayout navigation requires currentId");
    return navigationItems(navigation, currentId);
  });
  let openSectionId = $state<string | undefined>(untrack(() => navigation?.find(item => item.id === currentId && !item.href)?.id));
  let moreOpen = $state(false);
  let compact = $state(false);
  let navigationElement = $state<HTMLElement>();
  let headerElement = $state<HTMLElement>();
  let headerHeight = $state(0);
  let bottomElement = $state<HTMLElement>();
  let bottomHeight = $state(0);
  const section = $derived(primaryItems.find(item => item.id === openSectionId && !item.href));
  const overflowItems = $derived(primaryItems.length > 5 ? primaryItems.slice(4) : []);
  const mainItems = $derived(overflowItems.length ? primaryItems.slice(0,4) : primaryItems);
  const frame = $derived(responsiveNavigationLayout({side, dockable, header:header!==undefined, panel:section !== undefined}));
  const bottomItems = $derived.by((): readonly BottomNavigationItem[] => {
    const entries = mainItems.map(item => ({...item, expanded:item.href ? undefined : panelOpen && !moreOpen && section?.id === item.id, controls:item.href ? undefined : menuId, onselect:() => activate(item)}));
    if (!overflowItems.length) return entries;
    const badge = overflowItems.reduce((total,item) => total + (typeof item.badge === "number" ? item.badge : 0),0);
    return [...entries,{id:moreId,label:moreLabel,fallback:"···",expanded:panelOpen && moreOpen,controls:menuId,badge:badge || undefined,onselect:() => {moreOpen=true; setPanelOpen(true);}}];
  });
  const bottomCurrent = $derived(mainItems.some(item => item.id === currentId) ? currentId! : moreId);
  const menuTitle = $derived(moreOpen ? moreLabel : section?.label ?? sidebarLabel);

  function setPanelOpen(next: boolean): void {
    if (panelOpen === next) return;
    panelOpen = next;
    onpanelopenchange?.(next);
  }

  function activate(item: PrimaryNavigationItem): void {
    if (item.disabled) return;
    item.onselect?.();
    moreOpen = false;
    if (item.href) {openSectionId=undefined; setPanelOpen(false);}
    else {
      openSectionId=item.id; setPanelOpen(true);
      if (compact) void tick().then(() => panelOpen && document.getElementById(menuId)?.querySelector<HTMLElement>("[data-dialog-body] input:not(:disabled), [data-dialog-body] a[href], [data-dialog-body] button:not(:disabled)")?.focus());
    }
  }

  function closeNavigation(): void {
    setPanelOpen(false);
    moreOpen = false;
    if (!compact) openSectionId = undefined;
  }

  function closeAfterDestination(event: MouseEvent): void {
    if (event.button || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    const anchor = event.target instanceof Element ? event.target.closest("a[href]") : null;
    if (anchor && anchor.getAttribute("target") !== "_blank" && !anchor.hasAttribute("download")) setPanelOpen(false);
  }

  let previousCurrentId = $state(untrack(() => currentId));
  $effect(() => {
    if (!navigation || currentId === previousCurrentId) return;
    previousCurrentId = currentId;
    openSectionId = navigation.find(item => item.id === currentId && !item.href)?.id;
    moreOpen = false;
    setPanelOpen(false);
  });

  $effect(() => {
    if (!headerElement) return;
    const element=headerElement;
    const measure=() => {headerHeight=element.getBoundingClientRect().height;};
    measure();
    if(typeof ResizeObserver === "undefined") return;
    const observer=new ResizeObserver(measure); observer.observe(element);
    return () => observer.disconnect();
  });

  $effect(() => {
    if (!bottomElement || !navigation) return;
    const element=bottomElement;
    const measure = () => { const height=element.getBoundingClientRect().height; if(height) bottomHeight=height; };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  });

  $effect(() => {
    if (!navigation || typeof matchMedia !== "function") return;
    const step = getComputedStyle(document.documentElement).getPropertyValue("--reddb-breakpoint-md").trim();
    const query = matchMedia(`(max-width: calc(${step} - 0.02px))`);
    compact = query.matches;
    const resize = () => {
      const wasOpen=panelOpen;
      compact=query.matches; setPanelOpen(false); moreOpen=false;
      if (wasOpen && !compact) void tick().then(() => [...navigationElement?.querySelectorAll<HTMLElement>("[data-primary-navigation-rail] [data-sidebar-rail-item]") ?? []].find(element => element.dataset.sidebarRailItem === (openSectionId ?? currentId))?.focus());
    };
    const back = () => {setPanelOpen(false); moreOpen=false;};
    query.addEventListener("change",resize);
    window.addEventListener("popstate",back);
    return () => {query.removeEventListener("change",resize); window.removeEventListener("popstate",back);};
  });

  function closePanel(): void {
    if (!panelOpen) return;
    panelOpen = false;
    onpanelopenchange?.(false);
  }

  function handleWindowKeydown(event: KeyboardEvent): void {
    if (hasRail && event.key === "Escape") closePanel();
  }
</script>

<svelte:window onkeydown={handleWindowKeydown} />

{#snippet sidebarRegion()}
  <aside
    aria-label={sidebarLabel}
    class={hasRail ? slots.panel() : slots.region({ class: slots.sidebar() })}
    data-sidebar-layout-region="sidebar"
    inert={hasRail && !panelOpen}
    data-mobile-presentation={hasRail ? "drawer" : undefined}
    data-state={hasRail ? (panelOpen ? "open" : "closed") : undefined}
  >
    <Stack {gap}>{@render sidebar?.({ item: undefined, close: closePanel })}</Stack>
  </aside>
{/snippet}

{#snippet railRegion()}
  <div bind:this={railElement} class={slots.railRegion()} data-sidebar-layout-region="rail" hidden={!railOpen}>
    {@render rail?.()}
  </div>
{/snippet}

{#snippet drawerBackdrop()}
  <!-- Viewport-fixed drawer chrome (rail layout only): its `md:` step is the
       page chrome's, the one place ADR 0021 lets a Kit query the viewport. -->
  {#if hasRail && panelOpen}
    <button
      type="button"
      aria-label="Close sidebar panel"
      data-sidebar-drawer-backdrop
      class="fixed inset-0 z-30 bg-foreground/20 md:hidden"
      onclick={closePanel}
    ></button>
  {/if}
{/snippet}

{#snippet mainRegion()}
  <svelte:element
    this={rendersMain ? "main" : "div"}
    id={regionId}
    tabindex={regionId ? -1 : undefined}
    class={hasRail ? slots.main() : slots.region({ class: slots.content() })}
    data-sidebar-layout-region="main"
  >
    <Stack {gap}>{@render children?.()}</Stack>
  </svelte:element>
{/snippet}

{#snippet contextualMenu()}
  <!-- Links inside the caller's menu close the mobile drawer after a normal activation. -->
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class={frame.menu()} onclick={closeAfterDestination} data-primary-contextual-menu>
    {@render sidebar?.({item:section,close:closeNavigation})}
  </div>
{/snippet}

{#if navigation}
  <div
    bind:this={navigationElement}
    {...(rest as Record<string,unknown>)}
    data-sidebar-layout
    data-sidebar-layout-mode="navigation"
    data-side={side}
    class={frame.root({class:className})}
    style:--_reddb-navigation-bottom-height={dockable && compact && bottomHeight ? `${bottomHeight}px` : "0px"}
    style:--_reddb-navigation-header-height={`${headerHeight}px`}
    style:padding-bottom={dockable && compact && bottomHeight ? `${bottomHeight}px` : undefined}
  >
    {#if header}<div bind:this={headerElement} class={frame.header()} data-primary-navigation-header>{@render header()}</div>{/if}
    <div class={frame.rail()} style:height={headerHeight ? `calc(100dvh - ${headerHeight}px)` : undefined} data-primary-navigation-rail>
      <SidebarRail
        label={navigationLabel}
        showLabels={showRailLabels}
        items={primaryItems.map(item => ({...item,expanded:item.href ? undefined : section?.id === item.id,controls:item.href ? undefined : menuId}))}
        {currentId}
        onselect={id => {const item=primaryItems.find(item=>item.id===id); if(item) activate(item);}}
      />
    </div>
    {#if !compact && section}
      <aside id={menuId} aria-label={section.label} class={frame.panel()} style:height={headerHeight ? `calc(100dvh - ${headerHeight}px)` : undefined} data-primary-navigation-panel>
        <div class="flex items-center justify-between gap-[var(--reddb-spatial-gap-md)] border-b border-muted p-[var(--reddb-spatial-inset-md)]">
          <h2 class="m-0 text-heading">{section.label}</h2>
          <Button variant="ghost" size="sm" aria-label={`Close ${section.label}`} onclick={closeNavigation}>Close</Button>
        </div>
        {@render contextualMenu()}
      </aside>
    {/if}
    <svelte:element this={rendersMain ? "main" : "div"} id={regionId} tabindex={regionId ? -1 : undefined} class={frame.main()} data-sidebar-layout-region="main">
      <Stack {gap}>{@render children?.()}</Stack>
    </svelte:element>
    {#if dockable}
      <div bind:this={bottomElement} class={frame.bottom()} data-primary-navigation-bottom>
        <BottomNavigation label={navigationLabel} items={bottomItems} currentId={bottomCurrent} />
      </div>
    {/if}
    {#if compact}
      <Drawer
        id={menuId}
        side={side === "start" ? "left" : "right"}
        title={menuTitle}
        bind:open={panelOpen}
        onopenchange={onpanelopenchange}
        class={frame.drawer()}
        data-primary-navigation-drawer
      >
        {#if moreOpen}
          <nav aria-label={moreLabel} class={frame.more()}>
            {#each overflowItems as item (item.id)}
              {#snippet glyph()}{#if item.icon}<Icon icon={item.icon} size="md" color="current" aria-hidden="true" />{/if}{/snippet}
              {#snippet markers()}
                {#if item.badge !== undefined}<Badge tone={item.badgeTone ?? "neutral"} aria-hidden="true">{navigationBadge(item.badge)}</Badge>{/if}
                {#if item.status}<StatusIndicator label={item.status.label} tone={item.status.tone} showLabel={false} aria-hidden="true" />{/if}
              {/snippet}
              <NavItem label={item.label} aria-label={navigationMarkerLabel(item,item.label)} href={item.href} current={item.id === currentId} disabled={item.disabled} icon={item.icon ? glyph : undefined} trailing={markers} data-primary-overflow-item={item.id} onclick={() => activate(item)} />
            {/each}
          </nav>
        {:else}{@render contextualMenu()}{/if}
      </Drawer>
    {/if}
  </div>
{:else}

<div
  {...(rest as Record<string, unknown>)}
  data-sidebar-layout
  style:--_reddb-sidebar-rail-width={hasRail && railWidth ? `${railWidth}px` : undefined}
  data-side={side}
  data-sidebar-layout-mode={hasRail ? "rail-panel" : "panel"}
  data-rail-open={hasRail ? railOpen : undefined}
  data-panel-open={hasRail ? panelOpen : undefined}
  class={slots.root({ class: className })}
>
  {#if side === "start"}
    {#if hasRail}{@render railRegion()}{/if}
    {@render drawerBackdrop()}
    {@render sidebarRegion()}
    {@render mainRegion()}
  {:else}
    {@render mainRegion()}
    {@render sidebarRegion()}
    {@render drawerBackdrop()}
    {#if hasRail}{@render railRegion()}{/if}
  {/if}
</div>

{/if}
