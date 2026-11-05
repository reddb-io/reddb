<script lang="ts">
  let quickview: HTMLDialogElement;

  function openQuickview(): void {
    quickview.setAttribute("open", "");
    quickview.querySelector<HTMLElement>("button")?.focus();
  }

  function closeQuickview(event: KeyboardEvent): void {
    if (event.key !== "Escape") return;
    event.preventDefault();
    quickview.removeAttribute("open");
  }
</script>

<!-- Deliberately unnamed: neither the collection nor its product action has a name. -->
<ul data-unnamed-product-list>
  <li>
    <button type="button" data-unnamed-product-action>
      <svg aria-hidden="true" viewBox="0 0 16 16"><path d="M2 8h12" /></svg>
    </button>
  </li>
</ul>

<!-- Deliberately incomplete: dismissal strands focus inside the closed dialog. -->
<button type="button" data-broken-quickview-trigger onclick={openQuickview}>Quick view</button>
<dialog bind:this={quickview} data-broken-quickview onkeydown={closeQuickview}>
  <button type="button" data-broken-quickview-action>Add to basket</button>
</dialog>
