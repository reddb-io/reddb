<!-- Issue 483: links rendered by several Base components, and by the caller, inside one NavigationScope. -->
<script lang="ts">
  import { Breadcrumbs, Link, NavigationScope, Pagination } from "@reddb-io/design-system/base";

  interface Props {
    navigate: (href: string, event: MouseEvent) => void;
    scoped?: boolean;
  }

  const { navigate, scoped = true }: Props = $props();
</script>

{#snippet links()}
  <Link href="/collections" data-probe="link">Collections</Link>
  <Breadcrumbs
    items={[
      { id: "home", label: "Home", href: "/" },
      { id: "db", label: "Database", href: "/databases/main" },
      { id: "here", label: "Settings", current: true },
    ]}
  />
  <Pagination
    pages={[
      { page: 1, href: "/rows?page=1", current: true },
      { page: 2, href: "/rows?page=2" },
    ]}
  />
  <a href="/caller-owned" data-probe="caller">Caller anchor</a>
  <a href="https://example.com/docs" data-probe="external">External</a>
  <a href="/report.csv" download data-probe="download">Download</a>
  <a href="/elsewhere" target="_blank" data-probe="blank">New tab</a>
  <a href="#section" data-probe="fragment">Fragment</a>
  <a href="/native" data-native-navigation data-probe="opt-out">Opt out</a>
{/snippet}

{#if scoped}
  <NavigationScope {navigate}>{@render links()}</NavigationScope>
{:else}
  {@render links()}
{/if}
