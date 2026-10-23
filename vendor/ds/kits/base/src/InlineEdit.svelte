<script lang="ts">
  import { tick, untrack } from "svelte";
  import Button from "./Button.svelte";
  import Input from "./Input.svelte";
  import Icon, { type IconGlyph } from "./Icon.svelte";
  import { inlineEdit } from "./inline-edit.variants";
  import { createInlineEditSession, inlineEditReadValue, type InlineEditContract } from "./core/inline-edit";

  interface Props extends InlineEditContract {
    /** Optional decorative glyph, rendered through Icon in both the read view and editor. */
    icon?: IconGlyph;
    /** Reports a committed value after validation and persistence succeed. */
    onvaluechange?: (value: string) => void;
    /** Reports entry into or exit from editing. */
    oneditingchange?: (editing: boolean) => void;
    /** Optional persistence operation. A rejection keeps the draft open. */
    onconfirm?: (value: string) => void | Promise<void>;
    /** Extra classes on the field group. */
    class?: string;
  }
  let { label, icon, presentation = "text", editVisibility = "always", sourceLabel, savedLabel = "Saved", inputType = "text", value = $bindable(), onvaluechange, editing = $bindable(), defaultEditing = false, oneditingchange,
    validate, onconfirm, disabled = false, required = true, maxlength, emptyLabel = "Untitled",
    editLabel = "Edit", saveLabel = "Save", cancelLabel = "Cancel", requiredMessage = "Enter a value.",
    saveErrorMessage = "Could not save. Try again.", class: className }: Props = $props();
  const id = $props.id();
  const styles = $derived(inlineEdit({ editVisibility, presentation, withIcon: Boolean(icon) }));
  let localEditing = $state(untrack(() => defaultEditing));
  const active = $derived(editing ?? localEditing);
  const initial = untrack(() => value);
  let draftState = $state({ draft: initial, error: "", saving: false });
  const session = createInlineEditSession(initial, next => { draftState = next; });
  let input = $state<HTMLInputElement>();
  let readRow = $state<HTMLDivElement>();
  let previous = { active: false, value: initial };
  let restoreFocus = false;
  $effect(() => {
    const next = { active, value }, was = previous;
    if (next.value !== was.value || next.active !== was.active) session.reset(next.value);
    if (next.active && (!was.active || next.value !== was.value)) void tick().then(() => { input?.focus(); input?.select(); });
    if (!next.active && restoreFocus) { restoreFocus = false; void tick().then(() => readRow?.querySelector<HTMLButtonElement>("button")?.focus()); }
    previous = next;
  });
  $effect(() => () => session.invalidate());
  function setEditing(next: boolean) {
    if (!next) restoreFocus = true;
    editing = next; localEditing = next; oneditingchange?.(next);
  }
  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (disabled) return;
    const result = await session.save({ required, requiredMessage, saveErrorMessage, validate, confirm: onconfirm });
    if (result.status === "committed") { value = result.value; onvaluechange?.(result.value); setEditing(false); }
    else if (result.status === "invalid" || result.status === "failed") { await tick(); input?.focus(); }
  }
  function cancel() { if (!session.state.saving) { session.reset(value); setEditing(false); } }
</script>

<div role="group" aria-labelledby={`${id}-label`} aria-describedby={presentation === "input" && !active ? `${id}-status` : undefined} class={styles.root({ class: className })} data-inline-edit data-presentation={presentation} data-state={active ? (draftState.saving ? "saving" : "editing") : "saved"}>
  <span id={`${id}-label`} class={styles.label()}>{label}</span>
  {#if active}
    <form class={styles.row()} aria-labelledby={`${id}-label`} aria-busy={draftState.saving} onsubmit={save} novalidate>
      <div class={styles.field()}>
        {#if icon}<span aria-hidden="true" class={styles.inputIcon()}><Icon {icon} size="sm" color="current" focusable="false" /></span>{/if}
        <Input bind:ref={input} type={inputType} controlSize={presentation === "input" ? "md" : "sm"} value={draftState.draft} onvaluechange={next => session.change(String(next ?? ""))}
          aria-labelledby={`${id}-label`} aria-invalid={draftState.error ? "true" : undefined}
          aria-describedby={draftState.error ? `${id}-error` : undefined} {maxlength} {required} disabled={disabled || draftState.saving}
          class={styles.input()} onkeydown={event => {
            if (event.key === "Escape" && !event.isComposing && !session.state.saving) { event.preventDefault(); event.stopPropagation(); cancel(); }
            if (event.key === "Enter" && event.isComposing) event.preventDefault();
          }} />
      </div>
      <Button type="submit" size={presentation === "input" ? "md" : "sm"} disabled={disabled || draftState.saving} loading={draftState.saving}>{saveLabel}</Button>
      <Button variant="ghost" size={presentation === "input" ? "md" : "sm"} disabled={draftState.saving} onclick={cancel}>{cancelLabel}</Button>
    </form>
    {#if draftState.error}<p id={`${id}-error`} role="alert" class={styles.error()}>{draftState.error}</p>{/if}
  {:else}
    <div bind:this={readRow} class={styles.row()}>
      <span data-inline-edit-value class={styles.value()}>
        {#if icon}<span aria-hidden="true" class={styles.icon()}><Icon {icon} size="sm" color="current" focusable="false" /></span>{/if}
        <span class={styles.readText()}>{inlineEditReadValue(value, inputType, emptyLabel)}</span>
      </span>
      <Button size={presentation === "input" ? "md" : "sm"} variant="ghost" class={styles.edit()} {disabled} aria-label={`${editLabel} ${label}`} onclick={() => setEditing(true)}>{editLabel}</Button>
    </div>
    {#if presentation === "input"}<p id={`${id}-status`} data-saved-value role="status" class={styles.status()}>{savedLabel}{sourceLabel ? ` · ${sourceLabel}` : ""}</p>{/if}
  {/if}
</div>
