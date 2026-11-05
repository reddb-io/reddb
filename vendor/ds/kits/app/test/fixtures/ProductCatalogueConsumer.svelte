<script lang="ts">
  import {
    ProductFeature,
    ProductList,
    ProductOverview,
    ProductQuickview,
    type ProductListItem,
  } from "@reddb-io/design-system/app";

  const products: ProductListItem[] = [
    {
      id: "field-notes",
      name: "Field notes",
      href: "/products/field-notes",
      description: "A pocket notebook.",
      price: "$12",
    },
  ];
</script>

{#snippet notebookMedia()}
  <img src="/field-notes.jpg" alt="Black field-notes book" />
{/snippet}

{#snippet quickviewTrigger()}
  <span>Preview field notes</span>
{/snippet}

<div data-theme="base" data-color-scheme="dark" data-density="compact">
  <ProductList label="Stationery" items={products} columns={2} />

  <ProductOverview
    title="Field notes"
    description="A pocket notebook for everyday observations."
    price="$12"
    media={notebookMedia}
  >
    <p>Ships with dot-grid paper.</p>
    {#snippet actions()}<button type="button">Add to basket</button>{/snippet}
  </ProductOverview>

  <ProductFeature
    title="Built for the field"
    description="Thread-bound pages lie flat while you write."
    media={notebookMedia}
    mediaSide="end"
  />

  <ProductQuickview
    triggerLabel="Preview Field notes"
    title="Field notes"
    description="A pocket notebook."
    trigger={quickviewTrigger}
    media={notebookMedia}
  >
    <button type="button">Choose Field notes</button>
  </ProductQuickview>
</div>
