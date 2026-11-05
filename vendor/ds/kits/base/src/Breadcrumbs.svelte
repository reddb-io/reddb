<script module lang="ts">
  export type { BreadcrumbItem } from "./core/breadcrumbs";
</script>

<script lang="ts">
  import type { BreadcrumbItem } from "./core/breadcrumbs";
  import Link from "./Link.svelte";
  import { breadcrumbs } from "./breadcrumbs.variants";
  import { breadcrumbList, jsonLdScript } from "./structured-data";

  interface Props {
    /**
     * The trail in order. An item with `current` marks the page you are on; one without `href` is
     * plain text.
     */
    items: readonly BreadcrumbItem[];
    /** Accessible name of the navigation landmark. Defaults to `Breadcrumbs`. */
    label?: string;
    /**
     * Opt in to emitting the trail as a schema.org `BreadcrumbList` JSON-LD block (ADR 0028).
     * `true` keeps each `href` as written; `{ origin }` resolves root-relative hrefs to absolute
     * URLs, which search engines expect. Defaults to `false`, so a page that already emits the
     * trail elsewhere never emits it twice.
     */
    structuredData?: boolean | { origin: string };
    /** Extra classes merged onto the `nav` root. */
    class?: string;
  }

  let { items, label = "Breadcrumbs", structuredData = false, class: className }: Props = $props();
  const styles = breadcrumbs();
  const jsonLd = $derived(
    structuredData === false
      ? undefined
      : jsonLdScript(
          breadcrumbList(items, structuredData === true ? {} : { origin: structuredData.origin }),
        ),
  );
</script>

<nav aria-label={label} data-breadcrumbs class={styles.root({ class: className })}>
  <ol class={styles.list()}>
    {#each items as item, index (item.id)}
      <li class={styles.item()}>
        {#if item.current}
          <span aria-current="page" class={styles.current()}>{item.label}</span>
        {:else if item.href !== undefined}
          <Link href={item.href} class={styles.link()}>{item.label}</Link>
        {:else}
          <span>{item.label}</span>
        {/if}
        {#if index < items.length - 1}<span aria-hidden="true" class={styles.separator()}>/</span>{/if}
      </li>
    {/each}
  </ol>
  {#if jsonLd}{@html jsonLd}{/if}
</nav>
