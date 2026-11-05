<script lang="ts">
  import {
    DropdownMenu,
    Navbar,
    type DropdownMenuEntry,
    type NavbarAlign,
    type NavbarCollapse,
    type NavbarLink,
  } from "@reddb-io/design-system/base";

  interface Props {
    links?: readonly NavbarLink[];
    align?: NavbarAlign;
    collapse?: NavbarCollapse;
    open?: boolean;
    notifications?: readonly DropdownMenuEntry[];
    account?: readonly DropdownMenuEntry[];
  }

  let {
    links = [
      { id: "nodes", label: "Nodes", href: "/nodes", active: true },
      { id: "queries", label: "Queries", href: "/queries" },
      { id: "settings", label: "Settings", href: "/settings" },
    ],
    align,
    collapse,
    open = $bindable(false),
    notifications = [
      { id: "invite", label: "Invite accepted" },
      { id: "backup", label: "Backup finished" },
    ],
    account = [
      { id: "profile", label: "Profile", href: "/profile" },
      { heading: "Session", items: [{ id: "sign-out", label: "Sign out" }] },
    ],
  }: Props = $props();
</script>

<Navbar {links} {align} {collapse} bind:open>
  {#snippet brand()}
    <a href="/" aria-label="reddb.io home" data-brand>reddb.io</a>
  {/snippet}

  {#snippet actions()}
    <DropdownMenu
      triggerLabel="Notifications"
      contentLabel="Notifications"
      items={notifications}
    />
    <DropdownMenu triggerLabel="RD account" contentLabel="Account" items={account} variant="ghost">
      {#snippet trigger()}<span data-avatar>RD</span>{/snippet}
    </DropdownMenu>
  {/snippet}
</Navbar>
