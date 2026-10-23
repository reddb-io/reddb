<script lang="ts">
  import {
    DescriptionList,
    GridList,
    List,
    ListContainer,
    type DescriptionListItem,
  } from "@reddb-io/design-system/base";

  const details: readonly DescriptionListItem[] = [
    { term: "Region", detail: "South America" },
    { term: "Owner", detail: "Platform team" },
  ];
</script>

<div
  data-appearance-scope
  data-theme="marketing"
  data-color-scheme="dark"
  data-density="compact"
>
  <List data-nested-list aria-label="Deployments" gap="lg">
    <li><a href="/alpha">Alpha</a></li>
    <li><button type="button">Retry</button></li>
  </List>

  <ListContainer data-nested-list-container title="Services" padding="sm">
    <List aria-label="Services"><li>API</li></List>
  </ListContainer>

  <DescriptionList data-nested-description-list aria-label="Service details" items={details} />

  <GridList data-nested-grid-list aria-label="Projects" columns={2} gap="sm">
    <li>Design system</li>
    <li>Control plane</li>
  </GridList>
</div>
