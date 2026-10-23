<script lang="ts">
  import { CategoryPreview, Incentive } from "@reddb-io/design-system/app";
  import { Link } from "@reddb-io/design-system/base";
</script>

{#snippet categoryMedia()}
  <img src="/desks.jpg" alt="Oak writing desk" />
{/snippet}

{#snippet deliveryIcon()}
  <svg viewBox="0 0 24 24"><path d="M3 6h12v10H3z" /></svg>
{/snippet}

<div data-merchandising-consumer data-theme="base" data-color-scheme="dark" data-density="compact">
  <CategoryPreview
    name="Desks"
    href="/categories/desks"
    description="Workspaces for every room."
    media={categoryMedia}
  />
  <Incentive
    title="Complimentary delivery"
    description="Orders over $50 ship free."
    icon={deliveryIcon}
  >
    {#snippet actions()}<Link href="/delivery">Delivery details</Link>{/snippet}
  </Incentive>
</div>
