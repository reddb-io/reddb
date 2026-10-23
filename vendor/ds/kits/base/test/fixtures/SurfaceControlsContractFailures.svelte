<script lang="ts">
  interface Props {
    failure: "unreachable-scroll-area" | "unpersisted-appearance-switch";
    storageKey?: string;
  }

  const { failure, storageKey = "reddb:appearance:theme" }: Props = $props();

  function changeTheme(event: Event): void {
    const control = event.currentTarget as HTMLSelectElement;
    document.documentElement.setAttribute("data-theme", control.value);
    // Deliberately omits storage.setItem(storageKey, control.value).
    void storageKey;
  }
</script>

{#if failure === "unreachable-scroll-area"}
  <div data-broken-scroll-area class="overflow-y-auto">
    <p>First deployment</p>
    <p>Second deployment</p>
  </div>
{:else}
  <label>
    Theme
    <select data-broken-appearance-switch onchange={changeTheme}>
      <option value="application">Application</option>
      <option value="marketing">Marketing</option>
    </select>
  </label>
{/if}
