<!-- Native tabular relationships over caller-owned labels and data. -->
<script module lang="ts">
  export type { TableCellValue, TableColumn, TableRow, TableCellContext } from "./core/table";
</script>

<script lang="ts">
  import type { TableCellValue, TableColumn, TableRow, TableCellContext } from "./core/table";
  import type { Snippet } from "svelte";
  import type { HTMLTableAttributes } from "svelte/elements";
  import { table as tableAppearance } from "./table.variants";

  const uid = $props.id();

  interface Props extends Omit<HTMLTableAttributes, "children" | "class"> {
    /** Required visible name for the table. */
    caption: string;
    /** Columns rendered in this order with native column-header scope. */
    columns: readonly TableColumn[];
    /** Caller-owned tabular values. */
    rows: readonly TableRow[];
    /** Column key whose body cells are native row headers. Defaults to the first column. */
    rowHeader?: string;
    /** Caller-owned rendering for each cell; native cell relationships remain Table-owned. */
    cell?: Snippet<[TableCellContext]>;
    /** Extra classes merged onto the native table. */
    class?: string;
    /** Extra classes merged onto the responsive scroll region. */
    containerClass?: string;
    /** Extra classes merged onto the `<caption>`, over its own. */
    captionClass?: string;
    /** Extra classes merged onto the `<thead>`, over its own. */
    headerClass?: string;
    /** Extra classes merged onto every row, the header row and each body row. */
    rowClass?: string;
    /** Extra classes merged onto each column header cell. */
    headClass?: string;
    /** Extra classes merged onto each body cell, row-header cells included. */
    cellClass?: string;
  }

  let {
    caption,
    columns,
    rows,
    rowHeader = columns[0]?.key,
    cell,
    class: className,
    containerClass,
    captionClass,
    headerClass,
    rowClass,
    headClass,
    cellClass,
    ...rest
  }: Props = $props();
  const captionId = `${uid}-caption`;
  const styles = tableAppearance();
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex (wide tables need a keyboard-scrollable region.) -->
<div
  data-table-scroll
  role="region"
  aria-labelledby={captionId}
  tabindex="0"
  class={styles.root({ class: containerClass })}
>
  <table {...rest} data-table class={styles.table({ class: className })}>
    <caption id={captionId} class={styles.caption({ class: captionClass })}>{caption}</caption>
    <thead class={styles.header({ class: headerClass })}>
      <tr class={styles.row({ class: rowClass })}>
        {#each columns as column (column.key)}
          <th scope="col" class={styles.head({ class: headClass })}>{column.header}</th>
        {/each}
      </tr>
    </thead>
    <tbody>
      {#each rows as row, rowIndex}
        <tr class={styles.row({ class: rowClass })}>
          {#each columns as column (column.key)}
            {@const value = row[column.key]}
            {#if column.key === rowHeader}
              <th scope="row" class={styles.cell({ class: cellClass })}>
                {#if cell}{@render cell({ column, row, value, rowIndex })}{:else}{value}{/if}
              </th>
            {:else}
              <td class={styles.cell({ class: cellClass })}>
                {#if cell}{@render cell({ column, row, value, rowIndex })}{:else}{value}{/if}
              </td>
            {/if}
          {/each}
        </tr>
      {/each}
    </tbody>
  </table>
</div>
