<!--
  The Button as it was before issue #50 — the shape a consumer's strict
  svelte-check rejected, kept as a fixture so the check that now passes on the
  real Kit is demonstrably a check and not a formality.

  Three errors live here, and they are the three the consumer reported:
  `disabled` re-declared narrower than the interface it also inherits, and the
  two-armed `rest` spread that makes the element's props a union TypeScript
  will not compute. It is deliberately never imported by the Kit; only
  test/consumer-check.test.ts ever points a compiler at it.
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAnchorAttributes, HTMLButtonAttributes } from "svelte/elements";

  interface Common {
    disabled?: boolean;
    class?: string;
    children?: Snippet;
  }

  interface ButtonMode extends Common, Omit<HTMLButtonAttributes, "class"> {
    href?: never;
  }

  interface AnchorMode extends Common, Omit<HTMLAnchorAttributes, "class"> {
    href: string;
  }

  type Props = ButtonMode | AnchorMode;

  const { disabled = false, href, type, class: className, children, ...rest }: Props = $props();

  const tag = $derived(href !== undefined ? "a" : "button");
  const native = $derived(
    tag === "a"
      ? { href: disabled ? undefined : href, type, tabindex: disabled ? -1 : undefined }
      : { type: type ?? "button", disabled },
  );
</script>

<svelte:element
  this={tag}
  {...rest}
  {...native}
  class={className}
  aria-disabled={tag === "a" && disabled ? "true" : undefined}
>
  {@render children?.()}
</svelte:element>
