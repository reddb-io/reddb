<script lang="ts">
  import Swap from "../../src/Swap.svelte";
  import CodeBlock from "../../src/CodeBlock.svelte";
  let { name, options = {}, clicked, changed }: { name: string; options?: Record<string, any>; clicked?: () => void; changed?: (next: boolean) => void } = $props();
  export function update(next: Record<string, any>) { options = next; }
  const native = $derived.by(() => { const { faces, ...rest } = options; return rest; });
</script>
{#if name === "Swap"}
  {#if options.faces}
    <Swap {...native} label={options.label} onclick={clicked} onswappedchange={changed}>
      {#snippet off()}<span aria-hidden="true">◇</span> Pin{/snippet}
      {#snippet on()}<span aria-hidden="true">◆</span> Pin{/snippet}
    </Swap>
  {:else}<Swap {...native} label={options.label} onclick={clicked} onswappedchange={changed} />{/if}
{:else}<CodeBlock {...native} code={options.code} />{/if}
