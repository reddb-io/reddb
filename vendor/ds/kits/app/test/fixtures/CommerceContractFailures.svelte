<script lang="ts">
  let { failure }: { failure: "checkout-error-is-unassociated" | "cart-total-is-silent" } =
    $props();
</script>

{#if failure === "checkout-error-is-unassociated"}
  <form data-broken-checkout>
    <label for="postal-code">Postal code</label>
    <input id="postal-code" name="postalCode" aria-invalid="true" />
    <p id="postal-code-error" role="alert">Enter a postal code.</p>
  </form>
{:else}
  <form data-broken-cart>
    <p>Cart total</p>
    <output>$48.00</output>
  </form>
{/if}
