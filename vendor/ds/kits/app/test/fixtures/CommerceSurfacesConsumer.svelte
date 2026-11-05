<script lang="ts">
  import {
    CheckoutForm,
    OrderHistory,
    OrderSummary,
    ShoppingCart,
    type CheckoutSection,
    type OrderHistoryEntry,
    type OrderSummaryLine,
    type ShoppingCartItem,
  } from "@reddb-io/design-system/app";

  const sections: readonly CheckoutSection[] = [
    { legend: "Delivery", fields: [{ name: "name", label: "Full name", autocomplete: "name" }] },
  ];
  const items: readonly ShoppingCartItem[] = [
    { id: "keyboard", name: "Mechanical keyboard", price: "$48.00", quantity: 1 },
  ];
  const lines: readonly OrderSummaryLine[] = [{ label: "Subtotal", value: "$48.00" }];
  const orders: readonly OrderHistoryEntry[] = [
    { id: "RDB-1042", placed: "8 August 2026", placedAt: "2026-08-08", status: "Delivered", total: "$48.00" },
  ];
</script>

<div data-commerce-consumer data-theme="application" data-color-scheme="dark" data-density="compact">
  <ShoppingCart title="Shopping cart" {items} totalLabel="Total" total="$48.00" />
  <CheckoutForm {sections} submitLabel="Place order" aria-label="Checkout" />
  <OrderSummary title="Order summary" {lines} totalLabel="Total" total="$48.00" />
  <OrderHistory title="Order history" caption="Past orders" {orders} />
</div>
