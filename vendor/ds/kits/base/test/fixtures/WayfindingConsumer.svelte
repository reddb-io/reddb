<script lang="ts">
  import { Breadcrumbs, Navbar, Pagination, Steps } from "@reddb-io/design-system/base";

  const links = [
    { id: "overview", label: "Overview", href: "/overview", current: true },
    { id: "activity", label: "Activity", href: "/activity" },
  ] as const;
</script>

<section
  data-wayfinding-scope
  data-theme="marketing"
  data-color-scheme="dark"
  data-contrast="high"
  data-density="compact"
>
  <Navbar {links} collapse="expanded" />
  <Breadcrumbs
    items={[
      { id: "clusters", label: "Clusters", href: "/clusters" },
      { id: "primary", label: "Primary", current: true },
    ]}
  />
  <Pagination pages={[{ page: 1, href: "?page=1", current: true }]} />
  <Steps items={[{ id: "account", label: "Account", state: "current" }]} />
</section>
