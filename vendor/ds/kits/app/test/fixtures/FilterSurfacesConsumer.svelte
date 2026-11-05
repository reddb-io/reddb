<script lang="ts">
  import {
    CategoryFilter,
    Filter,
    type CategoryFilterOption,
    type FilterOption,
  } from "@reddb-io/design-system/app";

  const filters: readonly FilterOption[] = [
    { value: "all", label: "All" },
    { value: "available", label: "Available" },
  ];
  const categories: readonly CategoryFilterOption[] = [
    { value: "database", label: "Database" },
    { value: "cache", label: "Cache" },
  ];
  let active = $state("all");
  let selected = $state<string[]>(["database"]);
</script>

<div data-filter-consumer data-theme="application" data-color-scheme="dark" data-density="compact">
  <Filter legend="Availability" options={filters} bind:value={active} name="availability" />
  <CategoryFilter legend="Categories" options={categories} bind:value={selected} name="category" />
</div>
