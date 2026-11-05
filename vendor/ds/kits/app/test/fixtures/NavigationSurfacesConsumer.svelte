<script lang="ts">
  import {
    BottomNavigation,
    SidebarNavigation,
    StoreNavigation,
    type ApplicationNavigationItem,
  } from "@reddb-io/design-system/app";

  const items: readonly ApplicationNavigationItem[] = [
    { id: "home", label: "Home", href: "/" },
    { id: "catalogue", label: "Catalogue", href: "/catalogue" },
    { id: "account", label: "Account", href: "/account" },
  ];
</script>

<div data-theme="application" data-color-scheme="dark" data-density="compact">
  <SidebarNavigation label="Workspace" {items} currentId="catalogue" />
  <BottomNavigation label="Primary" {items} currentId="home" />
  <StoreNavigation label="Store" {items} currentId="catalogue" collapse="expanded">
    {#snippet brand()}<a href="/">Store home</a>{/snippet}
    {#snippet actions()}<a href="/cart">Cart</a>{/snippet}
  </StoreNavigation>
</div>
