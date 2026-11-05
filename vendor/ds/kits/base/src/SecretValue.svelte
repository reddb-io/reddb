<script lang="ts">
  import Button from "./Button.svelte";
  import { secretValue } from "./secret-value.variants";
  import { createSecretExposure, secretIsVisible } from "./core/secret-value";
  import type { SecretValueContract } from "./core/contracts/secretvalue";
  interface Props extends SecretValueContract {
    /** Additional composition classes. */
    class?: string;
  }
  let {
    label,
    value,
    revealable = false,
    revealed = false,
    pending = false,
    error,
    onrevealedchange,
    hideAfter = 30000,
    hiddenLabel = "Hidden value",
    revealLabel = "Reveal",
    hideLabel = "Hide",
    class: className,
  }: Props = $props();
  const s = secretValue(),
    id = $props.id();
  let exposed = $state(false);
  let root: HTMLDivElement;
  const options = () => ({
    label,
    value,
    revealable,
    revealed,
    pending,
    error,
    onrevealedchange,
    hideAfter,
  });
  const exposure = createSecretExposure(options, (next) => {
    exposed = next;
  });
  const visible = $derived(
    exposed && secretIsVisible(options()) && exposure.matches(value),
  );
  $effect(() => {
    exposure.sync();
  });
  $effect(() => {
    const document = root.ownerDocument;
    const changed = () => {
      if (document.visibilityState === "hidden") exposure.hide("visibility");
    };
    document.addEventListener("visibilitychange", changed);
    return () => {
      document.removeEventListener("visibilitychange", changed);
      exposure.destroy();
    };
  });
</script>

<div
  bind:this={root}
  data-secret-value
  data-revealed={visible}
  role="group"
  aria-labelledby={`${id}-label`}
  class={s.root({ class: className })}
>
  <span id={`${id}-label`} class={s.label()}>{label}</span>
  <div class={s.row()} aria-busy={pending}>
    <span data-secret-content class={s.value()}
      >{#if visible}{value}{:else}<span aria-hidden="true">••••••••</span><span
          class="sr-only">{hiddenLabel}</span
        >{/if}</span
    >
    {#if revealable && onrevealedchange}<Button
        variant="ghost"
        size="sm"
        loading={pending}
        aria-label={`${visible ? hideLabel : revealLabel} ${label}`}
        aria-pressed={visible}
        onclick={() => exposure.request(!visible)}
        >{visible ? hideLabel : revealLabel}</Button
      >{/if}
  </div>
  {#if error}<p role="alert" class={s.error()}>{error}</p>{/if}
</div>
