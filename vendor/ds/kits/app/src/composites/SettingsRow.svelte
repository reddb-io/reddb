<script lang="ts">
  import type { Snippet } from "svelte";
  import { Label } from "@reddb-io/design-system/base";
  import type { SettingsRowContract } from "../core/contracts/settingsrow";
  import { settingsRowAssociation, type FieldControlProps } from "../core/settings-row";
  import { settingsRow } from "./settings-row.variants";

  interface Props extends SettingsRowContract {
    /** Field content receiving only its native id, requirement and description/error association. */
    children?: Snippet<[FieldControlProps]>;
    /** Independently named actions or controls. Their visible labels are never overwritten by the row. */
    actions?: Snippet;
    /** Additional classes on the row's container. */
    class?: string;
  }
  let { label, description, error, required = false, id, children, actions, class: className }: Props = $props();
  const generated = $props.id();
  const association = $derived(settingsRowAssociation({ label, description, error, required, id: id ?? `${generated}-control` }));
  const styles = settingsRow();
</script>

<div data-settings-row class={styles.root({ class: className })} role="group" aria-labelledby={association.labelId}>
  <div class={styles.row()}>
    <div class={styles.copy()}>
      {#if children}
        <Label id={association.labelId} for={association.control.id} class={styles.label()}>{label}{#if required}<span class={styles.required()} aria-hidden="true">*</span><span class="sr-only"> (required)</span>{/if}</Label>
      {:else}
        <p id={association.labelId} class={styles.label()}>{label}</p>
      {/if}
      {#if description}<p id={association.helpId} class={styles.description()}>{description}</p>{/if}
      {#if error}<p id={association.errorId} class={styles.error()} role="alert">{error}</p>{/if}
    </div>
    <div class={styles.end()}>
      {#if children}<div data-settings-row-control class={styles.control()}>{@render children(association.control)}</div>{/if}
      {#if actions}<div data-settings-row-actions class={styles.actions()}>{@render actions()}</div>{/if}
    </div>
  </div>
</div>
